//! Conversion between VM heap values and backend-neutral callback state.

use kira_runtime_abi::NativeStateValue;

use super::{Heap, Object, Value};

impl Heap {
    /// Moves a runtime value into the backend-neutral callback-state tree.
    pub fn into_native_state(&mut self, value: Value) -> Option<NativeStateValue> {
        Some(match value {
            Value::Int(value) => NativeStateValue::Int(value),
            Value::Float(value) => NativeStateValue::Float(value),
            Value::Bool(value) => NativeStateValue::Bool(value),
            Value::RawPtr(value) => NativeStateValue::RawPtr(value),
            Value::Str(id) => match self.take_object(id.0) {
                Some(Object::Str(value)) => NativeStateValue::String(value),
                _ => return None,
            },
            Value::Struct(id) => {
                let fields = match self.take_object(id.0) {
                    Some(Object::Struct(fields)) => fields,
                    _ => return None,
                };
                let mut values = Vec::with_capacity(fields.len());
                for field in fields {
                    values.push(self.into_native_state(field)?);
                }
                NativeStateValue::Struct(values)
            }
            Value::Array(id) => {
                // Moving the elements out is a write like any other: an array
                // sharing them would be left reading values this took over.
                self.make_array_unique(id);
                let elements = match self.take_object(id.0) {
                    Some(Object::Array(elements)) => elements,
                    _ => return None,
                };
                // Sole owner by now, and the slot just gave up its hold.
                let Ok(elements) = std::rc::Rc::try_unwrap(elements) else {
                    return None;
                };
                let mut values = Vec::with_capacity(elements.len());
                for element in elements {
                    values.push(self.into_native_state(element)?);
                }
                NativeStateValue::Array(values)
            }
            // Moving the payload out is the one thing an enum is never asked to
            // do elsewhere, and a shared object cannot answer it: the values
            // still holding it would be left reading what this took. They get
            // the object; this gets a copy of the payload.
            Value::Enum(id) => {
                let (tag, payload) = match self.enum_shares(id.0) {
                    Some(1) | None => match self.take_object(id.0) {
                        Some(Object::Enum { tag, payload, .. }) => (tag, payload),
                        _ => return None,
                    },
                    Some(_) => {
                        let (tag, payload) = match self.slots.get(id.0 as usize) {
                            Some(Some(Object::Enum { tag, payload, .. })) => (*tag, *payload),
                            _ => return None,
                        };
                        let payload = payload.map(|value| self.copy_value(value));
                        self.free_enum(super::EnumId(id.0));
                        (tag, payload)
                    }
                };
                NativeStateValue::Enum {
                    tag,
                    payload: match payload {
                        Some(value) => Some(Box::new(self.into_native_state(value)?)),
                        None => None,
                    },
                }
            }
            Value::Void | Value::NativeState(_) | Value::NativeView { .. } => {
                return None;
            }
        })
    }

    /// Moves a backend-neutral callback-state tree into this heap.
    pub fn from_native_state(&mut self, value: NativeStateValue) -> Value {
        match value {
            NativeStateValue::Int(value) => Value::Int(value),
            NativeStateValue::Float(value) => Value::Float(value),
            NativeStateValue::Bool(value) => Value::Bool(value),
            NativeStateValue::RawPtr(value) => Value::RawPtr(value),
            NativeStateValue::String(value) => Value::Str(self.alloc(value)),
            NativeStateValue::Struct(fields) => {
                let fields = fields
                    .into_iter()
                    .map(|field| self.from_native_state(field))
                    .collect();
                Value::Struct(self.alloc_struct(fields))
            }
            NativeStateValue::Array(elements) => {
                let elements = elements
                    .into_iter()
                    .map(|element| self.from_native_state(element))
                    .collect();
                Value::Array(self.alloc_array(elements))
            }
            NativeStateValue::Enum { tag, payload } => {
                let payload = payload.map(|value| self.from_native_state(*value));
                Value::Enum(self.alloc_enum(tag, payload))
            }
        }
    }

    fn take_object(&mut self, index: u32) -> Option<Object> {
        let object = self.slots.get_mut(index as usize)?.take()?;
        self.freed += 1;
        self.free_list.push(index);
        Some(object)
    }
}
