//! Full userdata payloads and host userdata types (ADR 0042, ADR 0044,
//! ADR 0045).
//!
//! A full userdata's payload is either bytes, which Lua cannot read and a
//! snapshot copies exactly, or a Rust value of a type the host registered.
//! The Rust value is owned by its userdata object: it goes when the object
//! is collected, and nothing else refers to it. Every access to it goes
//! through this module, by `TypeId`-checked downcast; the VM never sees
//! its type.
//!
//! Rust `Drop` is not Lua's `__gc`. A host value is dropped when the
//! collector frees its userdata, or when the runtime goes, at a point no
//! Lua program observes and in no order Lua defines. Cleanup a program can
//! observe belongs to Lua finalizers.

use crate::HostEnv;
use std::any::{Any, TypeId};

/// Maximum serialized external key for rebindable userdata.
pub const MAX_REBIND_KEY: usize = 4096;

/// A registered host type's snapshot policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UserdataPolicy {
    /// Restore through a deterministic byte codec.
    Portable,
    /// Refuse snapshots containing this type.
    Refuse,
    /// Restore a bounded external key against the host environment.
    Rebind,
}

/// A resource could not be rebound. The message describes the host failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RebindError(pub &'static str);

impl std::fmt::Display for RebindError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}
impl std::error::Error for RebindError {}

/// A host handle whose stable key is resolved against resources already owned
/// by the host. Rebinding receives no runtime, Lua context, or journal.
pub trait RebindUserdata: HostUserdata + Sized {
    /// Return a deterministic external key, at most [`MAX_REBIND_KEY`] bytes.
    fn key(&self) -> Vec<u8>;
    /// Resolve the key against the host's resources before a runtime exists.
    fn rebind(key: &[u8], env: &HostEnv) -> Result<Self, RebindError>;
}

/// The most bytes a byte payload holds: the longest string.
pub(crate) const MAX_USERDATA_BYTES: usize = crate::heap::STRING_CEILING;

/// The most user values a full userdata has. Lua's API takes fewer than
/// `USHRT_MAX`.
pub(crate) const MAX_USER_VALUES: usize = u16::MAX as usize - 1;

/// A Rust type whose values Lua can hold as full userdata.
///
/// Registering the type with [`crate::HostRegistry::register_userdata`]
/// makes it snapshot-refusing: a snapshot of a heap holding one fails with
/// [`crate::SnapshotError::NonPortableUserdata`]. Registering it with
/// [`crate::HostRegistry::register_portable_userdata`] makes it portable
/// through its [`PortableUserdata`] codec. Registering it with
/// [`crate::HostRegistry::register_rebind_userdata`] uses its
/// [`RebindUserdata`] key and the host resource environment instead.
///
/// A host value holds no Lua values: the collector cannot see inside it.
/// A value that must keep Lua values alive keeps them in its userdata's
/// user values, or in host roots. Owned [`crate::AnyUserData`] values root the
/// payload; checked borrow guards cannot cross mutable execution. See
/// [`crate::Runtime::create_host_userdata`] and [`crate::AnyUserData::borrow_mut`].
///
/// ```
/// use moonseed::{HostRegistry, HostUserdata, Runtime, SnapshotError};
/// struct Counter(i64);
/// impl HostUserdata for Counter {
///     const SYMBOL: &'static str = "example.Counter";
///     fn logical_size(&self) -> u64 { 8 }
/// }
/// let mut registry = HostRegistry::new();
/// registry.register_userdata::<Counter>(); // refuses snapshots
/// let mut rt = Runtime::builder().registry(registry).build()?;
/// let counter = rt.create_host_userdata(Counter(41), 0)?;
/// counter.borrow_mut::<Counter>(&mut rt)?.0 += 1;
/// assert_eq!(counter.borrow::<Counter>(&rt)?.0, 42);
/// assert_eq!(rt.snapshot(), Err(SnapshotError::NonPortableUserdata));
/// # Ok::<(), moonseed::Error>(())
/// ```
pub trait HostUserdata: 'static {
    /// The type's stable name: what a snapshot records, and what argument
    /// errors name. It must not change between a snapshot and its restore.
    const SYMBOL: &'static str;

    /// The logical bytes a value counts against the heap quota, besides
    /// its userdata object. Moonseed cannot see a Rust value's own
    /// allocations. Mutable [`crate::UserDataRefMut`] guards remeasure on drop.
    /// Legacy clients report growth through `NativeCall::set_userdata_charge`.
    fn logical_size(&self) -> u64;
}

/// A host userdata type with a deterministic codec.
///
/// `encode` must give the same bytes on every target for the same value,
/// and `decode` must accept what `encode` gives. A snapshot's bytes are not
/// trusted: `decode` gets at most `MAX_USERDATA_BYTES` bytes and returns
/// `None` for anything it does not accept, which fails the restore.
pub trait PortableUserdata: HostUserdata + Sized {
    /// Encode this payload deterministically, without host resources or Lua references.
    fn encode(&self) -> Vec<u8>;
    /// Validate and decode untrusted payload bytes; None refuses restore.
    fn decode(bytes: &[u8]) -> Option<Self>;
}

/// A registered host userdata type.
#[derive(Clone, Copy)]
pub(crate) struct HostType {
    pub(crate) symbol: &'static str,
    pub(crate) type_id: TypeId,
    /// `logical_size`, with the type erased.
    pub(crate) size: fn(&dyn Any) -> Option<u64>,
    pub(crate) policy: Policy,
}

#[derive(Clone, Copy)]
pub(crate) enum Policy {
    Refuse,
    Portable(Codec),
    Rebind {
        key: fn(&dyn Any) -> Option<Vec<u8>>,
        rebind: fn(&[u8], &HostEnv) -> Result<Decoded, RebindError>,
    },
}

impl Policy {
    pub(crate) fn kind(self) -> UserdataPolicy {
        match self {
            Self::Refuse => UserdataPolicy::Refuse,
            Self::Portable(_) => UserdataPolicy::Portable,
            Self::Rebind { .. } => UserdataPolicy::Rebind,
        }
    }
}

/// A decoded host value and the logical bytes it declares.
pub(crate) type Decoded = (Box<dyn Any>, u64);

/// A portable type's codec, with its type erased.
#[derive(Clone, Copy)]
pub(crate) struct Codec {
    pub(crate) encode: fn(&dyn Any) -> Option<Vec<u8>>,
    pub(crate) decode: fn(&[u8]) -> Option<Decoded>,
}

impl HostType {
    pub(crate) fn of<T: HostUserdata>() -> Self {
        Self {
            symbol: T::SYMBOL,
            type_id: TypeId::of::<T>(),
            size: size_as::<T>,
            policy: Policy::Refuse,
        }
    }

    pub(crate) fn portable<T: PortableUserdata>() -> Self {
        Self {
            policy: Policy::Portable(Codec {
                encode: encode_as::<T>,
                decode: decode_as::<T>,
            }),
            ..Self::of::<T>()
        }
    }

    pub(crate) fn rebind<T: RebindUserdata>() -> Self {
        Self {
            policy: Policy::Rebind {
                key: key_as::<T>,
                rebind: rebind_as::<T>,
            },
            ..Self::of::<T>()
        }
    }
}

fn key_as<T: RebindUserdata>(value: &dyn Any) -> Option<Vec<u8>> {
    value.downcast_ref::<T>().map(T::key)
}

fn rebind_as<T: RebindUserdata>(key: &[u8], env: &HostEnv) -> Result<Decoded, RebindError> {
    let value = T::rebind(key, env)?;
    let size = value.logical_size();
    Ok((Box::new(value), size))
}

fn size_as<T: HostUserdata>(value: &dyn Any) -> Option<u64> {
    value.downcast_ref::<T>().map(T::logical_size)
}

fn encode_as<T: PortableUserdata>(value: &dyn Any) -> Option<Vec<u8>> {
    value.downcast_ref::<T>().map(T::encode)
}

fn decode_as<T: PortableUserdata>(bytes: &[u8]) -> Option<Decoded> {
    let value = T::decode(bytes)?;
    let size = value.logical_size();
    Some((Box::new(value), size))
}

/// What a full userdata holds besides its user values.
pub(crate) enum Payload {
    /// Internal file identity, independent of host-registered userdata.
    File(Box<crate::iolib::FileState>),
    /// Bytes Lua cannot read; zeroed when made.
    Bytes(Box<[u8]>),
    /// A value of a registered host type.
    Host {
        symbol: &'static str,
        value: Box<dyn Any>,
    },
}

impl Payload {
    /// The byte payload, if that is what this is.
    pub(crate) fn bytes(&self) -> Option<&[u8]> {
        match self {
            Payload::Bytes(bytes) => Some(bytes),
            Payload::Host { .. } | Payload::File(_) => None,
        }
    }

    pub(crate) fn bytes_mut(&mut self) -> Option<&mut [u8]> {
        match self {
            Payload::Bytes(bytes) => Some(bytes),
            Payload::Host { .. } | Payload::File(_) => None,
        }
    }

    /// The host value, if it is a `T`.
    pub(crate) fn host<T: 'static>(&self) -> Option<&T> {
        match self {
            Payload::Host { value, .. } => value.downcast_ref::<T>(),
            Payload::Bytes(_) | Payload::File(_) => None,
        }
    }

    pub(crate) fn host_mut<T: 'static>(&mut self) -> Option<&mut T> {
        match self {
            Payload::Host { value, .. } => value.downcast_mut::<T>(),
            Payload::Bytes(_) | Payload::File(_) => None,
        }
    }

    /// The host type's symbol, for a host value.
    pub(crate) fn symbol(&self) -> Option<&'static str> {
        match self {
            Payload::Host { symbol, .. } => Some(symbol),
            Payload::Bytes(_) | Payload::File(_) => None,
        }
    }
}

impl std::fmt::Debug for Payload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Payload::File(file) => write!(f, "File({})", file.id.0),
            Payload::Bytes(bytes) => write!(f, "Bytes({})", bytes.len()),
            Payload::Host { symbol, .. } => write!(f, "Host({symbol})"),
        }
    }
}

/// A light userdata key the host chooses (ADR 0043). Moonseed promises
/// only identity: two light userdata are equal when their keys are, and
/// never equal a token the VM makes. The bits are not an address; a host
/// that maps keys to its own resources keeps that map. Unstable API.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct HostLightKey(pub u64);

/// A light userdata as the host sees it: an opaque, copyable identity.
/// A host key comes back as it went in; a token the VM made can be
/// handed back in, never forged. Unstable API.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct LightUserdata {
    pub(crate) domain: crate::value::LightDomain,
    pub(crate) bits: u64,
}

impl LightUserdata {
    /// Construct a light userdata identity from a host-selected key.
    pub fn host(key: HostLightKey) -> Self {
        Self {
            domain: crate::value::LightDomain::Host,
            bits: key.0,
        }
    }

    /// The host key, if the host made this one.
    pub fn host_key(&self) -> Option<HostLightKey> {
        (self.domain == crate::value::LightDomain::Host).then_some(HostLightKey(self.bits))
    }
}
