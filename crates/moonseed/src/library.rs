//! The `math` and `table` libraries (ADR 0032, ADR 0033): which functions
//! exist, their registry symbols, the library state a runtime keeps, and
//! the progress of a table function (or `math.min` / `math.max`) that runs
//! over more than one step. The VM implements them in `runtime/library.rs`.

use crate::host::{Builtin, HostRegistry};

/// A `math` function.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MathFn {
    Abs,
    Acos,
    Asin,
    Atan,
    Ceil,
    Cos,
    Deg,
    Exp,
    Floor,
    Fmod,
    Log,
    Max,
    Min,
    Modf,
    Rad,
    Random,
    RandomSeed,
    Sin,
    Sqrt,
    Tan,
    ToInteger,
    Type,
    Ult,
}

/// A `table` function.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TableFn {
    Concat,
    Insert,
    Move,
    Pack,
    Remove,
    Sort,
    Unpack,
}

/// The `math` functions: field name, registry symbol, function.
pub(crate) const MATH_FUNCTIONS: [(&str, &str, MathFn); 23] = [
    ("abs", "math.abs", MathFn::Abs),
    ("acos", "math.acos", MathFn::Acos),
    ("asin", "math.asin", MathFn::Asin),
    ("atan", "math.atan", MathFn::Atan),
    ("ceil", "math.ceil", MathFn::Ceil),
    ("cos", "math.cos", MathFn::Cos),
    ("deg", "math.deg", MathFn::Deg),
    ("exp", "math.exp", MathFn::Exp),
    ("floor", "math.floor", MathFn::Floor),
    ("fmod", "math.fmod", MathFn::Fmod),
    ("log", "math.log", MathFn::Log),
    ("max", "math.max", MathFn::Max),
    ("min", "math.min", MathFn::Min),
    ("modf", "math.modf", MathFn::Modf),
    ("rad", "math.rad", MathFn::Rad),
    ("random", "math.random", MathFn::Random),
    ("randomseed", "math.randomseed", MathFn::RandomSeed),
    ("sin", "math.sin", MathFn::Sin),
    ("sqrt", "math.sqrt", MathFn::Sqrt),
    ("tan", "math.tan", MathFn::Tan),
    ("tointeger", "math.tointeger", MathFn::ToInteger),
    ("type", "math.type", MathFn::Type),
    ("ult", "math.ult", MathFn::Ult),
];

/// The `table` functions: field name, registry symbol, function.
pub(crate) const TABLE_FUNCTIONS: [(&str, &str, TableFn); 7] = [
    ("concat", "table.concat", TableFn::Concat),
    ("insert", "table.insert", TableFn::Insert),
    ("move", "table.move", TableFn::Move),
    ("pack", "table.pack", TableFn::Pack),
    ("remove", "table.remove", TableFn::Remove),
    ("sort", "table.sort", TableFn::Sort),
    ("unpack", "table.unpack", TableFn::Unpack),
];

/// Register the `math` functions under their `math.*` symbols.
pub fn register_math(registry: &mut HostRegistry) {
    for (_, symbol, function) in MATH_FUNCTIONS {
        registry.register_builtin(symbol, Builtin::Math(function));
    }
}

/// Register the `table` functions under their `table.*` symbols.
pub fn register_table(registry: &mut HostRegistry) {
    for (_, symbol, function) in TABLE_FUNCTIONS {
        registry.register_builtin(symbol, Builtin::Table(function));
    }
}

/// Register the base library, `math`, `table`, and `string`: every
/// function `Runtime::install_standard` installs.
pub fn register_standard(registry: &mut HostRegistry) {
    crate::base::register_base(registry);
    crate::oslib::register_os(registry);
    register_math(registry);
    register_table(registry);
    crate::strlib::register_string(registry);
    crate::utf8lib::register_utf8(registry);
    crate::package::register_package(registry);
    crate::corolib::register_coroutine(registry);
    crate::iolib::register_io(registry);
}

/// Library state a runtime keeps for its life (ADR 0032). Snapshot state:
/// it is the same on every target and replays exactly.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LibraryState {
    /// `math.random`'s xoshiro256** state, as Lua 5.4 keeps it. Never all
    /// zero: seeding sets a nonzero word.
    pub(crate) rng: [u64; 4],
    /// The deterministic entropy stream that seeds the generator when
    /// `math` is installed and when `math.randomseed()` gets no argument
    /// and the host gives no entropy: a splitmix64 counter, started from
    /// `Config::entropy`.
    pub(crate) entropy: u64,
}

impl LibraryState {
    pub(crate) fn new(entropy: u64) -> Self {
        let mut state = Self {
            rng: [0; 4],
            entropy,
        };
        state.seed(0, 0);
        state
    }

    /// The next word of the entropy stream (splitmix64).
    pub(crate) fn draw(&mut self) -> u64 {
        self.entropy = self.entropy.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.entropy;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// Lua 5.4's `setseed`: the seeds, a fixed nonzero word, and sixteen
    /// discarded outputs to spread them.
    pub(crate) fn seed(&mut self, n1: u64, n2: u64) {
        self.rng = [n1, 0xff, n2, 0];
        for _ in 0..16 {
            self.next();
        }
    }

    /// xoshiro256**, as Lua 5.4's `nextrand`.
    pub(crate) fn next(&mut self) -> u64 {
        let [s0, s1, s2, s3] = self.rng;
        let s2 = s2 ^ s0;
        let s3 = s3 ^ s1;
        let result = s1.wrapping_mul(5).rotate_left(7).wrapping_mul(9);
        self.rng = [s0 ^ s3, s1 ^ s2, s2 ^ (s1 << 17), s3.rotate_left(45)];
        result
    }

    /// Lua 5.4's `project`: a value in `0..=n` without bias. When `n + 1`
    /// is not a power of two, draws are masked to the smallest `2^b - 1`
    /// not below `n` and redrawn until one fits.
    pub(crate) fn project(&mut self, mut random: u64, n: u64) -> u64 {
        if n & n.wrapping_add(1) == 0 {
            return random & n;
        }
        let mut lim = n;
        lim |= lim >> 1;
        lim |= lim >> 2;
        lim |= lim >> 4;
        lim |= lim >> 8;
        lim |= lim >> 16;
        lim |= lim >> 32;
        loop {
            random &= lim;
            if random <= n {
                return random;
            }
            random = self.next();
        }
    }
}

/// Lua 5.4's `I2d`: the top 53 bits of an output as a float in `[0, 1)`.
pub(crate) fn unit_float(random: u64) -> f64 {
    (random >> 11) as f64 * (0.5 / (1u64 << 52) as f64)
}

/// A table function, or `math.min` / `math.max`, in progress (ADR 0033):
/// its work so far and what the call its frame made is for. It holds no
/// Lua values: those are in the frame's scratch slots, on the stack.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct LibTask {
    pub(crate) work: Work,
    pub(crate) wait: Wait,
}

/// What a library frame's call stands in for, and so what its result is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Wait {
    /// No call: the frame's last step ran out of its operations.
    Nothing,
    /// `__index`: the result goes to scratch slot `into`.
    Get { into: u32 },
    /// `__newindex`: the result is dropped.
    Set,
    /// `__len`: the result is the length.
    Len,
    /// `__lt`, `__eq`, or `table.sort`'s function: the result's truth.
    Truth,
    /// A call whose first two results go to scratch slots `into` and
    /// `into + 1`: a `package.searchers` entry (ADR 0039).
    Pair { into: u32 },
}

/// Where a library function is. Scratch slots are numbered from 0, above
/// the function's arguments.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Work {
    /// `math.min` / `math.max`: argument `best` wins so far; `next` is
    /// compared with it.
    Extreme {
        max: bool,
        best: u32,
        next: u32,
    },
    /// `table.insert`.
    Insert {
        stage: Stage,
        pos: i64,
        i: i64,
    },
    /// `table.remove`: the removed value is scratch 0.
    Remove {
        stage: Stage,
        size: i64,
        pos: i64,
    },
    /// `table.move`: element `i` of `n` goes from `f + i` to `t + i`.
    Move {
        stage: Stage,
        f: i64,
        t: i64,
        n: i64,
        i: i64,
        backward: bool,
    },
    /// `table.concat`: the text so far, charged to the logical heap.
    Concat {
        stage: Stage,
        i: i64,
        last: i64,
        text: Vec<u8>,
    },
    /// `table.pack`: the new table is scratch 0; `next` is the next
    /// argument to store, one past the last for the `n` field.
    Pack {
        stage: Stage,
        next: u32,
    },
    /// `table.unpack`: `count` results from `first`, `got` read so far,
    /// each in its scratch slot.
    Unpack {
        stage: Stage,
        first: i64,
        count: u32,
        got: u32,
    },
    /// `table.sort`.
    Sort(Box<SortState>),
    /// A `string` function (ADR 0034).
    Str(Box<crate::strlib::StrWork>),
    /// Bounded UTF-8 work.
    Utf8(Box<crate::utf8lib::Utf8Work>),
    Os(Box<crate::oslib::OsWork>),
    /// `require` or a `package` searcher (ADR 0039).
    Package(Box<crate::package::PackageWork>),
    /// `debug.traceback` (ADR 0040).
    Debug(Box<crate::debuglib::DebugWork>),
}

/// A table function's step, beyond what its counters say.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Stage {
    /// Nothing done yet.
    Start,
    /// The list's length is being taken.
    Length,
    /// Reading the first element (`table.remove`'s result).
    First,
    /// Reading the next element.
    Read,
    /// Writing the element just read.
    Write,
    /// Writing the final element.
    Last,
    /// `table.move` deciding the direction by `__eq`.
    Equal,
    /// Finished.
    Done,
}

/// `table.sort`'s quicksort, Lua 5.4.9's `auxsort` and `partition` with
/// its recursion made a stack of ranges (ADR 0033). Scratch 0 holds the
/// pivot or `a[lo]`, 1 and 2 the elements being compared.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SortState {
    pub(crate) step: SortStep,
    /// The list's length when the sort began: every index is in `1..=n`.
    pub(crate) n: u32,
    pub(crate) lo: u32,
    pub(crate) up: u32,
    pub(crate) p: u32,
    pub(crate) i: u32,
    pub(crate) j: u32,
    /// Pivot choice for large ranges, 0 for the middle one.
    pub(crate) rnd: u32,
    /// Ranges still to sort, the next last. Each holds the size of the
    /// range sorted before it, to test the partition's balance.
    pub(crate) pending: Vec<SortRange>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SortRange {
    pub(crate) lo: u32,
    pub(crate) up: u32,
    pub(crate) smaller: u32,
    pub(crate) rnd: u32,
}

/// Whether restore may make a native closure run by `builtin` with these
/// values and numbers (ADR 0035): only a shape the builtin itself makes.
pub(crate) fn closure_fits(
    heap: &crate::heap::Heap,
    builtin: crate::host::Builtin,
    values: &[crate::value::Value],
    state: &[i64],
) -> bool {
    match builtin {
        crate::host::Builtin::String(crate::strlib::StrFn::GmatchStep) => {
            crate::runtime::gmatch_fits(heap, values, state)
        }
        // `require` keeps the `package` table.
        crate::host::Builtin::Package(
            crate::package::PkgFn::Require | crate::package::PkgFn::SearchLua,
        ) => matches!(values, [crate::value::Value::Table(_)]) && state.is_empty(),
        // A `coroutine.wrap` function keeps its coroutine.
        crate::host::Builtin::Coroutine(crate::corolib::CoFn::WrapCall) => {
            matches!(values, [crate::value::Value::Thread(_)]) && state.is_empty()
        }
        crate::host::Builtin::Io(crate::iolib::IoFn::LinesStep) => {
            crate::runtime::io::lines_fits(heap, values, state)
        }
        _ => false,
    }
}

/// The most ranges a sort keeps pending. It always sorts the smaller
/// part first, so the stack is at most one entry per halving of 2^31.
pub(crate) const MAX_SORT_PENDING: usize = 40;

/// Each step of `table.sort`, named by the operation it makes. `Range`
/// makes none: it starts the current range or takes the next.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SortStep {
    Length,
    Range,
    GetLo,
    GetUp,
    UpLessLo,
    SetLoUp,
    SetUpLo,
    GetP,
    GetLo2,
    PLessLo,
    SetPLo,
    SetLoP,
    GetUp2,
    UpLessP,
    SetPUp,
    SetUpP,
    GetPivot,
    GetUpMinus1,
    SetPToUpMinus1,
    SetUpMinus1ToPivot,
    GetI,
    ILessPivot,
    GetJ,
    PivotLessJ,
    SetIJ,
    SetJI,
    SetUpMinus1I,
    SetIPivot,
}

impl Work {
    /// The scratch slots the work keeps above the function's arguments.
    pub(crate) fn scratch(&self) -> u32 {
        match self {
            Self::Extreme { .. } => 0,
            Self::Insert { .. } | Self::Move { .. } | Self::Concat { .. } | Self::Pack { .. } => 1,
            Self::Remove { .. } => 2,
            Self::Unpack { count, .. } => *count,
            Self::Str(work) => work.scratch(),
            Self::Utf8(work) => work.scratch(),
            Self::Os(_) => 10,
            Self::Package(work) => work.scratch(),
            Self::Debug(work) => work.scratch(),
            Self::Sort(_) => 3,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The output of xoshiro256** from the reference implementation's
    /// state `[1, 2, 3, 4]`.
    #[test]
    fn xoshiro_matches_the_reference_outputs() {
        let mut state = LibraryState::new(0);
        state.rng = [1, 2, 3, 4];
        let outputs: Vec<u64> = (0..3).map(|_| state.next()).collect();
        assert_eq!(outputs, [11520, 0, 1509978240]);
    }

    #[test]
    fn projection_stays_in_range_without_modulo() {
        let mut state = LibraryState::new(7);
        for n in [0u64, 1, 2, 5, 6, 7, 1000, u64::MAX / 3, u64::MAX] {
            for _ in 0..200 {
                let random = state.next();
                assert!(state.project(random, n) <= n);
            }
        }
    }
}
