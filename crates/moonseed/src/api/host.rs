use std::any::{Any, TypeId};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use crate::{HostRegistry, Libraries, Limits, SnapshotError};

/// Host-owned resources, indexed by Rust type. Resources are never serialized.
/// Cloning an environment shares its resources; insertion changes only that map.
#[derive(Clone, Default)]
pub struct HostEnv {
    resources: HashMap<TypeId, Rc<dyn Any>>,
}

impl HostEnv {
    /// Create an empty resource environment.
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert or replace a resource of this type.
    pub fn insert<T: 'static>(&mut self, resource: T) {
        self.resources.insert(TypeId::of::<T>(), Rc::new(resource));
    }

    /// Borrow a resource of the requested type.
    pub fn get<T: 'static>(&self) -> Option<&T> {
        self.resources.get(&TypeId::of::<T>())?.downcast_ref()
    }
}

/// Whether module resolution is immutable or an external journaled effect.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResolverPolicy {
    /// Resolution is deterministic and immutable for the run's lifetime.
    Pure,
    /// Record the result bytes and replay them without calling the resolver.
    External,
}

/// A host resolver's answer. Names, source, and diagnostics are byte strings.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Resolved {
    /// Lua source, compiled with the compiler's limits.
    Source(Vec<u8>),
    /// A Moonseed binary chunk, checked with `load`'s validation and budget.
    Binary(Vec<u8>),
    /// A registered native loader's stable symbol.
    Native(String),
    /// A diagnostic appended to `require`'s list of searcher messages.
    NotFound(Vec<u8>),
}

/// A host module capability. It receives no runtime, Lua context, or journal.
/// Implementations must bound their work and return exact bytes in [`Resolved`].
/// Source/binary validation and loader errors are handled by Lua `require`.
/// The resolver object is host state; reattach it through [`HostCapabilities`]
/// after restore. [`ResolverPolicy::Pure`] requires immutable deterministic
/// answers; `External` records resolution bytes in the host's [`crate::Journal`].
///
/// ```
/// use moonseed::{Libraries, ModuleResolver, Resolved, ResolverPolicy, Runtime};
/// struct Modules;
/// impl ModuleResolver for Modules {
///     fn resolve(&self, name: &[u8]) -> Resolved {
///         match name {
///             b"answer" => Resolved::Source(b"return 42".to_vec()),
///             _ => Resolved::NotFound(b"module not supplied".to_vec()),
///         }
///     }
/// }
/// let rt = Runtime::builder().libraries(Libraries::PACKAGE)
///     .module_resolver(ResolverPolicy::Pure, Modules).build()?;
/// # Ok::<(), moonseed::Error>(())
/// ```
pub trait ModuleResolver: 'static {
    /// Return a loader or a diagnostic for the exact module name bytes.
    fn resolve(&self, name: &[u8]) -> Resolved;
}

type Output = Rc<RefCell<crate::host::Output>>;
type Warnings = Rc<RefCell<crate::host::Warnings>>;
type Entropy = Rc<RefCell<crate::host::Entropy>>;

/// Optional host capabilities, shared by construction and restore.
/// They are execution resources and are never snapshot state. Registry symbols
/// and userdata policies are required to decode; sinks and resolvers are optional.
/// Defaults grant no external authority, independently of [`Libraries`]. Shared
/// backends need not be Send/Sync. A VM snapshot never serializes these objects:
/// retain/reconstruct live resource tables and reattach them through [`Host`].
/// Missing or denied operations report structured [`crate::HostIoError`]s;
/// rebind validation may fail with [`SnapshotError::NonPortableResource`].
///
/// ```
/// use moonseed::{HostCapabilities, Libraries, Runtime};
/// let rt = Runtime::builder().libraries(Libraries::STANDARD)
///     .capabilities(HostCapabilities::sandbox()).build()?;
/// # Ok::<(), moonseed::Error>(())
/// ```
#[derive(Clone, Default)]
pub struct HostCapabilities {
    pub(crate) output: Option<Output>,
    pub(crate) warnings: Option<Warnings>,
    pub(crate) entropy: Option<Entropy>,
    pub(crate) module_resolver: Option<(ResolverPolicy, Rc<dyn ModuleResolver>)>,
    pub(crate) env: HostEnv,
    /// Optional filesystem authority; never serialized.
    pub filesystem: Option<std::sync::Arc<dyn crate::Filesystem>>,
    /// Optional stdio authority; never serialized.
    pub stdio: Option<std::sync::Arc<dyn crate::Stdio>>,
    /// Optional clock authority; never serialized.
    pub clock: Option<std::sync::Arc<dyn crate::Clock>>,
    /// Optional civil authority; never serialized.
    pub civil: Option<std::sync::Arc<dyn crate::CivilTime>>,
    /// Optional environment authority; never serialized.
    pub environment: Option<std::sync::Arc<dyn crate::Environment>>,
    /// Optional process authority; never serialized.
    pub process: Option<std::sync::Arc<dyn crate::Process>>,
}

impl HostCapabilities {
    /// Supply independent filesystem authority.
    pub fn filesystem(mut self, capability: std::sync::Arc<dyn crate::Filesystem>) -> Self {
        self.filesystem = Some(capability);
        self
    }
    /// Supply independent stdio authority.
    pub fn stdio(mut self, capability: std::sync::Arc<dyn crate::Stdio>) -> Self {
        self.stdio = Some(capability);
        self
    }
    /// Supply independent clock authority.
    pub fn clock(mut self, capability: std::sync::Arc<dyn crate::Clock>) -> Self {
        self.clock = Some(capability);
        self
    }
    /// Supply independent civil authority.
    pub fn civil(mut self, capability: std::sync::Arc<dyn crate::CivilTime>) -> Self {
        self.civil = Some(capability);
        self
    }
    /// Supply independent environment authority.
    pub fn environment(mut self, capability: std::sync::Arc<dyn crate::Environment>) -> Self {
        self.environment = Some(capability);
        self
    }
    /// Supply independent process authority.
    pub fn process(mut self, capability: std::sync::Arc<dyn crate::Process>) -> Self {
        self.process = Some(capability);
        self
    }

    /// No ambient authority (the default profile).
    pub fn sandbox() -> Self {
        Self::default()
    }

    /// Set the output sink used by `print`.
    pub fn output(mut self, sink: impl FnMut(&[u8]) + 'static) -> Self {
        self.output = Some(Rc::new(RefCell::new(Box::new(sink))));
        self
    }
    /// Set the warning sink, including Lua's continuation flag.
    pub fn warnings(mut self, sink: impl FnMut(&[u8], bool) + 'static) -> Self {
        self.warnings = Some(Rc::new(RefCell::new(Box::new(sink))));
        self
    }
    /// Set the entropy source used by unseeded `math.randomseed`.
    pub fn entropy(mut self, source: impl FnMut() -> i64 + 'static) -> Self {
        self.entropy = Some(Rc::new(RefCell::new(Box::new(source))));
        self
    }
    /// Install the resolver after the preload searcher when package is built.
    pub fn module_resolver(
        mut self,
        policy: ResolverPolicy,
        resolver: impl ModuleResolver,
    ) -> Self {
        self.module_resolver = Some((policy, Rc::new(resolver)));
        self
    }
    /// Supply the resources used by rebindable userdata.
    pub fn host_env(mut self, env: HostEnv) -> Self {
        self.env = env;
        self
    }
    /// Inspect the host resource environment.
    pub fn env(&self) -> &HostEnv {
        &self.env
    }
}

/// Host registrations, limits, lineage, and optional capabilities for restore.
/// Host callback symbols and matching userdata policies must be registered before
/// decoding. Moonseed supplies its own allowed library symbols automatically;
/// this never installs new globals or grants [`HostCapabilities`] authority.
/// The journal and host resources remain host-owned; preserve them separately.
/// See [`crate::Runtime::restore`] for validation and root ownership.
///
/// ```
/// use moonseed::{Host, HostRegistry, Libraries, Runtime};
/// let original = Runtime::builder().libraries(Libraries::MATH).build()?;
/// let bytes = original.snapshot().unwrap();
/// let host = Host::new(HostRegistry::new()).libraries(Libraries::MATH);
/// let restored = Runtime::restore(&bytes, &host)?;
/// # Ok::<(), moonseed::Error>(())
/// ```
#[derive(Clone)]
#[non_exhaustive]
pub struct Host {
    /// Registry used to decode stable native and userdata symbols.
    pub registry: HostRegistry,
    /// Maximum resources the restored runtime may use.
    pub limits: Limits,
    /// Expected journal lineage. Defaults to the default config's domain, 1.
    pub effect_domain: u64,
    /// Host resources and optional execution capabilities.
    pub capabilities: HostCapabilities,
    /// Libraries permitted in the snapshot. Defaults to [`Libraries::ALL`].
    /// This allowlist also applies to manually registered library symbols.
    pub libraries: Libraries,
}

impl Host {
    /// Allow only these Moonseed libraries during restore. A snapshot retaining
    /// any other library symbol fails with [`SnapshotError::UnknownHostSymbol`]
    /// before userdata codecs or rebinds run. Missing allowed symbols use
    /// Moonseed's implementations; explicit host registrations retain precedence
    /// and must satisfy the snapshot's existing native policies/work shapes.
    /// This does not change the snapshot's globals or external authority.
    pub fn libraries(mut self, libraries: Libraries) -> Self {
        self.libraries = libraries;
        self
    }

    pub(crate) fn restore_registry(
        &self,
        symbols: &[String],
    ) -> std::result::Result<HostRegistry, SnapshotError> {
        let mut all = HostRegistry::new();
        Libraries::ALL.register(&mut all);
        let mut allowed = HostRegistry::new();
        self.libraries.register(&mut allowed);
        for symbol in symbols {
            if all.native_slot(symbol).is_some() && allowed.native_slot(symbol).is_none() {
                return Err(SnapshotError::UnknownHostSymbol);
            }
        }
        let mut registry = self.registry.clone();
        registry.fill_missing_natives(&allowed);
        Ok(registry)
    }

    /// Supply independent filesystem authority.
    pub fn filesystem(mut self, capability: std::sync::Arc<dyn crate::Filesystem>) -> Self {
        self.capabilities = self.capabilities.filesystem(capability);
        self
    }
    /// Supply independent stdio authority.
    pub fn stdio(mut self, capability: std::sync::Arc<dyn crate::Stdio>) -> Self {
        self.capabilities = self.capabilities.stdio(capability);
        self
    }
    /// Supply independent clock authority.
    pub fn clock(mut self, capability: std::sync::Arc<dyn crate::Clock>) -> Self {
        self.capabilities = self.capabilities.clock(capability);
        self
    }
    /// Supply independent civil authority.
    pub fn civil(mut self, capability: std::sync::Arc<dyn crate::CivilTime>) -> Self {
        self.capabilities = self.capabilities.civil(capability);
        self
    }
    /// Supply independent environment authority.
    pub fn environment(mut self, capability: std::sync::Arc<dyn crate::Environment>) -> Self {
        self.capabilities = self.capabilities.environment(capability);
        self
    }
    /// Supply independent process authority.
    pub fn process(mut self, capability: std::sync::Arc<dyn crate::Process>) -> Self {
        self.capabilities = self.capabilities.process(capability);
        self
    }

    /// Create a restore host with default limits, domain 1, all Moonseed library
    /// symbols allowed, and no host capabilities. For a restricted restore,
    /// explicitly select [`Self::libraries`]. Host symbols still require registration.
    pub fn new(registry: HostRegistry) -> Self {
        Self {
            registry,
            limits: Limits::default(),
            effect_domain: 1,
            capabilities: HostCapabilities::default(),
            libraries: Libraries::ALL,
        }
    }
    /// Set resource bounds for decoding and execution.
    pub fn limits(mut self, limits: Limits) -> Self {
        self.limits = limits;
        self
    }
    /// Require the snapshot to belong to this effect lineage.
    pub fn effect_domain(mut self, domain: u64) -> Self {
        self.effect_domain = domain;
        self
    }
    /// Replace all optional capabilities together.
    pub fn capabilities(mut self, capabilities: HostCapabilities) -> Self {
        self.capabilities = capabilities;
        self
    }
    /// Supply resources used by userdata rebinding.
    pub fn host_env(mut self, env: HostEnv) -> Self {
        self.capabilities = self.capabilities.host_env(env);
        self
    }
    /// Set the output sink used by `print` after restore.
    pub fn output(mut self, sink: impl FnMut(&[u8]) + 'static) -> Self {
        self.capabilities = self.capabilities.output(sink);
        self
    }
    /// Set the warning sink after restore.
    pub fn warnings(mut self, sink: impl FnMut(&[u8], bool) + 'static) -> Self {
        self.capabilities = self.capabilities.warnings(sink);
        self
    }
    /// Set the entropy source after restore.
    pub fn entropy(mut self, source: impl FnMut() -> i64 + 'static) -> Self {
        self.capabilities = self.capabilities.entropy(source);
        self
    }
    /// Set the resolver used by the snapshot's host searcher after restore.
    pub fn module_resolver(
        mut self,
        policy: ResolverPolicy,
        resolver: impl ModuleResolver,
    ) -> Self {
        self.capabilities = self.capabilities.module_resolver(policy, resolver);
        self
    }
}

impl Default for Host {
    fn default() -> Self {
        Self::new(HostRegistry::new())
    }
}
