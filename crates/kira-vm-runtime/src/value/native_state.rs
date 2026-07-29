//! Conversion between VM heap values and backend-neutral callback state.

use kira_runtime_abi::NativeStateValue;

use super::{Heap, Object, Value};

impl Heap {
    /// Moves a runtime value into the backend-neutral callback-state tree.
    ///
    /// The error names the shape that had no boxed form, as a noun phrase a
    /// message reads as a subject: a refusal deep inside a struct is otherwise
    /// indistinguishable from a refusal of the struct itself.
    pub fn into_native_state(&mut self, value: Value) -> Result<NativeStateValue, &'static str> {
        Ok(match value {
            Value::Int(value) => NativeStateValue::Int(value),
            Value::Float(value) => NativeStateValue::Float(value),
            Value::Bool(value) => NativeStateValue::Bool(value),
            Value::RawPtr(value) => NativeStateValue::RawPtr(value),
            Value::Str(id) => match self.take_object(id.0) {
                Some(Object::Str(value)) => NativeStateValue::String(value),
                _ => return Err("a string whose storage was already taken"),
            },
            Value::Struct(id) => {
                let fields = match self.take_object(id.0) {
                    Some(Object::Struct(fields)) => fields,
                    _ => return Err("a struct whose fields were already taken"),
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
                    _ => return Err("an array whose elements were already taken"),
                };
                // Sole owner by now, and the slot just gave up its hold.
                let Ok(elements) = std::rc::Rc::try_unwrap(elements) else {
                    return Err("an array still shared with another value");
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
                        _ => return Err("an enum whose payload was already taken"),
                    },
                    Some(_) => {
                        let (tag, payload) = match self.slots.get(id.0 as usize) {
                            Some(Some(Object::Enum { tag, payload, .. })) => (*tag, *payload),
                            _ => return Err("an enum whose payload was already taken"),
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
            Value::Void => return Err("a void value"),
            Value::Erased(_) => return Err("an `Any` inside callback state"),
            Value::Cell(_) => return Err("a captured `var` inside callback state"),
            Value::NativeState(_) => return Err("callback state inside callback state"),
            Value::NativeView { .. } => {
                return Err("a recovered callback-state view inside callback state");
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
