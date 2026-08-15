//! What a profile is: samples, the stacks they carry, and the frame table
//! those stacks index.
//!
//! One shape serves every backend and every collector. A VM run's Kira stacks,
//! a native executable's machine stacks, and the interpreter's own machine
//! stacks are all sequences of [`FrameId`] into one [`FrameTable`], weighted in
//! nanoseconds. That is what lets one renderer produce every report and one
//! reader compare two backends without learning two formats.

use std::collections::HashMap;

use kira_core::{Interner, Symbol};

/// A count of nanoseconds.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Nanos(u64);

impl Nanos {
    /// No time at all.
    pub const ZERO: Nanos = Nanos(0);

    /// A count of nanoseconds.
    #[must_use]
    pub const fn new(nanos: u64) -> Self {
        Self(nanos)
    }

    /// The raw nanosecond count.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }

    /// This many nanoseconds and `other` more, saturating rather than wrapping.
    #[must_use]
    pub const fn saturating_add(self, other: Nanos) -> Nanos {
        Nanos(self.0.saturating_add(other.0))
    }

    /// The time from `self` to `later`, or zero when `later` is not later.
    #[must_use]
    pub const fn until(self, later: Nanos) -> Nanos {
        Nanos(later.0.saturating_sub(self.0))
    }

    /// This duration as a fraction of `total`, or zero when `total` is zero.
    #[must_use]
    pub fn share_of(self, total: Nanos) -> f64 {
        if total.0 == 0 {
            return 0.0;
        }
        self.0 as f64 / total.0 as f64
    }
}

impl std::fmt::Display for Nanos {
    /// Renders a duration the way a report column wants it: three significant
    /// digits and the largest unit that keeps them.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let nanos = self.0 as f64;
        let (value, unit) = if self.0 >= 1_000_000_000 {
            (nanos / 1e9, "s")
        } else if self.0 >= 1_000_000 {
            (nanos / 1e6, "ms")
        } else if self.0 >= 1_000 {
            (nanos / 1e3, "us")
        } else {
            (nanos, "ns")
        };
        if value >= 100.0 {
            write!(formatter, "{value:.0}{unit}")
        } else if value >= 10.0 {
            write!(formatter, "{value:.1}{unit}")
        } else {
            write!(formatter, "{value:.2}{unit}")
        }
    }
}

/// What a sample's weight counts.
///
/// `perf` prints an event count whose unit is the event's; so does this. A
/// sampled profile weighs each sample by the time it stands for, and an exact
/// instruction profile weighs it by the instructions it stands for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Unit {
    /// Weights are nanoseconds.
    Nanoseconds,
    /// Weights are processor cycles.
    Cycles,
    /// Weights are interpreted instructions.
    Instructions,
}

impl Unit {
    /// The word a trace file records.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Nanoseconds => "nanoseconds",
            Self::Cycles => "cycles",
            Self::Instructions => "instructions",
        }
    }

    /// The unit a trace file's word names.
    #[must_use]
    pub fn parse(label: &str) -> Option<Self> {
        match label {
            "nanoseconds" => Some(Self::Nanoseconds),
            "cycles" => Some(Self::Cycles),
            "instructions" => Some(Self::Instructions),
            _ => None,
        }
    }

    /// A weight as a report prints it.
    #[must_use]
    pub fn render(self, weight: u64) -> String {
        match self {
            Self::Nanoseconds => Nanos::new(weight).to_string(),
            Self::Cycles | Self::Instructions => count(weight),
        }
    }
}

/// A large count as a report column prints it: three significant digits and a
/// magnitude suffix, so a billion cycles is a number a reader can take in.
#[must_use]
pub fn count(value: u64) -> String {
    let (scaled, suffix) = if value >= 1_000_000_000 {
        (value as f64 / 1e9, "G")
    } else if value >= 1_000_000 {
        (value as f64 / 1e6, "M")
    } else if value >= 1_000 {
        (value as f64 / 1e3, "K")
    } else {
        return value.to_string();
    };
    if scaled >= 100.0 {
        format!("{scaled:.0}{suffix}")
    } else if scaled >= 10.0 {
        format!("{scaled:.1}{suffix}")
    } else {
        format!("{scaled:.2}{suffix}")
    }
}

/// `part` as a fraction of `total`, or zero when `total` is zero.
#[must_use]
pub fn share(part: u64, total: u64) -> f64 {
    if total == 0 {
        return 0.0;
    }
    part as f64 / total as f64
}

/// Which thread of the profiled process a sample came from.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ThreadId(u32);

impl ThreadId {
    /// A thread's ordinal within one profile.
    #[must_use]
    pub const fn new(index: u32) -> Self {
        Self(index)
    }

    /// The ordinal.
    #[must_use]
    pub const fn index(self) -> u32 {
        self.0
    }
}

/// A frame interned in a [`FrameTable`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FrameId(u32);

impl FrameId {
    /// The frame's index in the table that made it.
    #[must_use]
    pub const fn index(self) -> u32 {
        self.0
    }
}

/// What kind of code a frame is, which decides how a report labels it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FrameKind {
    /// A function written in Kira.
    Kira,
    /// Kira's own runtime: the interpreter, the heap, the native bridge.
    Runtime,
    /// Other machine code in the program's own image.
    Native,
    /// A C library the program imported through `@FFI.Extern`.
    Foreign,
    /// The operating system and its shared libraries.
    System,
    /// An address no symbol covered.
    Unknown,
}

impl FrameKind {
    /// The one-character column a report prints before the symbol, in the
    /// place `perf` prints `[.]` and `[k]`.
    #[must_use]
    pub const fn marker(self) -> &'static str {
        match self {
            Self::Kira => "[K]",
            Self::Runtime => "[R]",
            Self::Native => "[.]",
            Self::Foreign => "[C]",
            Self::System => "[k]",
            Self::Unknown => "[?]",
        }
    }

    /// The word a trace file records.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Kira => "kira",
            Self::Runtime => "runtime",
            Self::Native => "native",
            Self::Foreign => "foreign",
            Self::System => "system",
            Self::Unknown => "unknown",
        }
    }

    /// The kind a trace file's word names.
    #[must_use]
    pub fn parse(label: &str) -> Option<Self> {
        Some(match label {
            "kira" => Self::Kira,
            "runtime" => Self::Runtime,
            "native" => Self::Native,
            "foreign" => Self::Foreign,
            "system" => Self::System,
            "unknown" => Self::Unknown,
            _ => return None,
        })
    }
}

/// One place a sample's stack can name.
///
/// A frame is identity, not an occurrence: two samples in the same function at
/// the same instruction share one frame, which is what keeps a long profile's
/// storage proportional to the program rather than to its running time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Frame {
    /// The symbol a report prints.
    pub symbol: Symbol,
    /// What kind of code this is.
    pub kind: FrameKind,
    /// The image the symbol came from: an executable, a shared library, or
    /// `[vm]` for a function the interpreter was running.
    pub object: Symbol,
    /// The Kira function index, for a frame that is a Kira function.
    pub function: Option<u32>,
    /// The offset inside the function: a bytecode instruction index for an
    /// interpreted frame, a byte offset for a machine frame.
    pub offset: Option<u32>,
    /// The source file, when the symbolizer knew one.
    pub file: Option<Symbol>,
    /// The source line, when the symbolizer knew one.
    pub line: Option<u32>,
}

impl Frame {
    /// The frame a table hands back for an index it does not hold.
    pub const UNKNOWN: Frame = Frame {
        symbol: Symbol::ERROR,
        kind: FrameKind::Unknown,
        object: Symbol::ERROR,
        function: None,
        offset: None,
        file: None,
        line: None,
    };

    /// A frame with nothing but a name and a kind.
    #[must_use]
    pub const fn named(symbol: Symbol, kind: FrameKind, object: Symbol) -> Self {
        Frame {
            symbol,
            kind,
            object,
            function: None,
            offset: None,
            file: None,
            line: None,
        }
    }

    /// The same frame without its instruction offset.
    ///
    /// What a report groups by when it wants one row per function rather than
    /// one row per instruction.
    #[must_use]
    pub const fn without_offset(mut self) -> Self {
        self.offset = None;
        self
    }
}

/// The interned frames and names one profile's samples index.
#[derive(Debug)]
pub struct FrameTable {
    names: Interner,
    frames: Vec<Frame>,
    index: HashMap<Frame, FrameId>,
}

impl Default for FrameTable {
    fn default() -> Self {
        Self::new()
    }
}

impl FrameTable {
    /// An empty table.
    #[must_use]
    pub fn new() -> Self {
        Self {
            names: Interner::new(),
            frames: Vec::new(),
            index: HashMap::new(),
        }
    }

    /// Interns a name, falling back to the reserved error name when the table
    /// has taken every handle a symbol can address.
    pub fn name(&mut self, text: &str) -> Symbol {
        self.names.intern(text).unwrap_or(Symbol::ERROR)
    }

    /// The text a symbol stands for.
    #[must_use]
    pub fn text(&self, symbol: Symbol) -> &str {
        self.names.resolve(symbol)
    }

    /// Adds `frame`, returning the id it already had when the table held it.
    pub fn insert(&mut self, frame: Frame) -> FrameId {
        if let Some(&id) = self.index.get(&frame) {
            return id;
        }
        let id = FrameId(self.frames.len() as u32);
        self.frames.push(frame);
        self.index.insert(frame, id);
        id
    }

    /// The id a trace file's raw index names, when this table holds it.
    #[must_use]
    pub fn id(&self, raw: u32) -> Option<FrameId> {
        ((raw as usize) < self.frames.len()).then_some(FrameId(raw))
    }

    /// The frame an id names.
    #[must_use]
    pub fn frame(&self, id: FrameId) -> &Frame {
        self.frames.get(id.0 as usize).unwrap_or(&Frame::UNKNOWN)
    }

    /// Every frame, in id order.
    #[must_use]
    pub fn frames(&self) -> &[Frame] {
        &self.frames
    }

    /// The symbol a frame prints under.
    #[must_use]
    pub fn symbol_of(&self, id: FrameId) -> &str {
        self.text(self.frame(id).symbol)
    }
}

/// One thread a profile has samples for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadRecord {
    /// The thread's ordinal, which samples carry.
    pub id: ThreadId,
    /// The name a report prints.
    pub name: String,
}

/// One observation: a stack, the moment it was taken, and the time it accounts
/// for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sample {
    /// The thread the stack belongs to.
    pub thread: ThreadId,
    /// When the sample was taken, from the start of recording.
    pub time: Nanos,
    /// What this sample accounts for, in the profile's unit: for a sampled
    /// profile, the interval since the previous sample of the same thread.
    pub weight: u64,
    /// The stack, outermost frame first.
    pub stack: Vec<FrameId>,
}

impl Sample {
    /// The innermost frame, which is where the time was actually spent.
    #[must_use]
    pub fn leaf(&self) -> Option<FrameId> {
        self.stack.last().copied()
    }
}

/// Which stacks a profile holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum View {
    /// Kira functions: what the program is doing.
    Kira,
    /// Machine frames: what the machine is doing to run it.
    Machine,
}

impl View {
    /// The word a trace file and a command line use.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Kira => "kira",
            Self::Machine => "machine",
        }
    }

    /// The view a word names.
    #[must_use]
    pub fn parse(label: &str) -> Option<Self> {
        match label {
            "kira" => Some(Self::Kira),
            "machine" => Some(Self::Machine),
            _ => None,
        }
    }
}

/// A complete set of samples over one view of one run.
#[derive(Debug)]
pub struct Profile {
    /// Which stacks these are.
    pub view: View,
    /// What was counted, in the place `perf` names an event.
    pub event: String,
    /// What a sample's weight counts.
    pub unit: Unit,
    /// How the collector described where the samples came from.
    pub collector: String,
    /// The requested sampling frequency, in hertz.
    pub frequency: u32,
    /// The threads samples name.
    pub threads: Vec<ThreadRecord>,
    /// The frames stacks index.
    pub frames: FrameTable,
    /// Every sample, in the order it was taken.
    pub samples: Vec<Sample>,
    /// Samples the collector knows it could not take or could not keep.
    pub lost: u64,
}

impl Profile {
    /// An empty profile of `view`.
    #[must_use]
    pub fn new(view: View, event: impl Into<String>, collector: impl Into<String>) -> Self {
        Self {
            view,
            event: event.into(),
            unit: Unit::Nanoseconds,
            collector: collector.into(),
            frequency: 0,
            threads: Vec::new(),
            frames: FrameTable::new(),
            samples: Vec::new(),
            lost: 0,
        }
    }

    /// The total weight of every sample.
    #[must_use]
    pub fn total(&self) -> u64 {
        self.samples
            .iter()
            .fold(0u64, |total, sample| total.saturating_add(sample.weight))
    }

    /// The name of a thread, or an ordinal when the collector named none.
    #[must_use]
    pub fn thread_name(&self, id: ThreadId) -> String {
        self.threads
            .iter()
            .find(|thread| thread.id == id)
            .map_or_else(
                || format!("thread-{}", id.index()),
                |thread| thread.name.clone(),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_frame_table_hands_the_same_id_back_for_the_same_frame() {
        let mut table = FrameTable::new();
        let name = table.name("Grid.step");
        let object = table.name("[vm]");
        let first = table.insert(Frame::named(name, FrameKind::Kira, object));
        let second = table.insert(Frame::named(name, FrameKind::Kira, object));
        assert_eq!(first, second);
        assert_eq!(table.frames().len(), 1);
        assert_eq!(table.symbol_of(first), "Grid.step");
    }

    #[test]
    fn an_index_no_frame_has_resolves_to_the_unknown_frame() {
        let table = FrameTable::new();
        assert_eq!(table.id(0), None);
        assert_eq!(table.frame(FrameId(9)), &Frame::UNKNOWN);
    }

    #[test]
    fn durations_render_with_the_unit_that_keeps_three_digits() {
        assert_eq!(Nanos::new(999).to_string(), "999ns");
        assert_eq!(Nanos::new(1_500).to_string(), "1.50us");
        assert_eq!(Nanos::new(12_500_000).to_string(), "12.5ms");
        assert_eq!(Nanos::new(2_000_000_000).to_string(), "2.00s");
    }

    #[test]
    fn a_profiles_total_is_the_weight_of_every_sample() {
        let mut profile = Profile::new(View::Kira, "kira-wall", "test");
        for _ in 0..3 {
            profile.samples.push(Sample {
                thread: ThreadId::new(0),
                time: Nanos::new(0),
                weight: 1_000,
                stack: Vec::new(),
            });
        }
        assert_eq!(profile.total(), 3_000);
        assert_eq!(profile.unit.render(profile.total()), "3.00us");
    }

    #[test]
    fn a_cycle_count_renders_with_its_magnitude() {
        assert_eq!(Unit::Cycles.render(999), "999");
        assert_eq!(Unit::Cycles.render(1_500), "1.50K");
        assert_eq!(Unit::Cycles.render(6_520_000_000), "6.52G");
    }
}
