use std::ops::{BitOr, BitOrAssign};

use crate::{Config, HostRegistry, Limits, Runtime};

use super::Result;

/// Standard libraries selected at construction or allowed on restore. `STANDARD` omits debug;
/// the default builder installs no libraries.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Libraries(u16);

impl Libraries {
    /// No libraries.
    pub const NONE: Self = Self(0);
    /// Base globals.
    pub const BASE: Self = Self(1);
    /// Package and require.
    pub const PACKAGE: Self = Self(2);
    /// Coroutine library.
    pub const COROUTINE: Self = Self(4);
    /// Math library.
    pub const MATH: Self = Self(8);
    /// Table library.
    pub const TABLE: Self = Self(16);
    /// String library and its type metatable.
    pub const STRING: Self = Self(32);
    /// Debug library.
    pub const DEBUG: Self = Self(64);
    /// Supported libraries other than debug.
    pub const STANDARD: Self = Self(959);
    /// Every supported library, including debug.
    pub const ALL: Self = Self(1023);
    /// UTF-8 byte operations.
    pub const UTF8: Self = Self(128);
    /// IO library; grants no host filesystem or stream authority.
    pub const IO: Self = Self(256);
    /// OS library; grants no authority.
    pub const OS: Self = Self(512);

    pub(crate) fn register(self, registry: &mut HostRegistry) {
        macro_rules! register {
            ($flag:ident, $function:path) => {
                if self.contains(Self::$flag) {
                    $function(registry);
                }
            };
        }
        register!(BASE, crate::register_base);
        register!(PACKAGE, crate::register_package);
        register!(COROUTINE, crate::register_coroutine);
        register!(MATH, crate::register_math);
        register!(TABLE, crate::register_table);
        register!(STRING, crate::register_string);
        register!(UTF8, crate::register_utf8);
        register!(DEBUG, crate::register_debug);
        register!(OS, crate::register_os);
        register!(IO, crate::register_io);
    }

    pub(crate) fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }
}

impl BitOr for Libraries {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}
impl BitOrAssign for Libraries {
    fn bitor_assign(&mut self, rhs: Self) {
        *self = *self | rhs;
    }
}

/// Construction policy and optional host capabilities (ADR 0053).
#[derive(Default)]
pub struct RuntimeBuilder {
    pub(crate) config: Config,
    pub(crate) registry: HostRegistry,
    pub(crate) libraries: Libraries,
    pub(crate) capabilities: super::HostCapabilities,
    pub(crate) package_path: Vec<u8>,
    pub(crate) package_cpath: Vec<u8>,
}

impl RuntimeBuilder {
    /// Set initial byte-string package paths. Paths grant no filesystem authority.
    /// Dynamic C loading is unsupported; cpath is exposed only as configuration.
    pub fn package_paths(mut self, path: impl AsRef<[u8]>, cpath: impl AsRef<[u8]>) -> Self {
        self.package_path = path.as_ref().to_vec();
        self.package_cpath = cpath.as_ref().to_vec();
        self
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

    /// Replace configuration, including any previously supplied limits.
    pub fn config(mut self, config: Config) -> Self {
        self.config = config;
        self
    }

    /// Replace resource limits in the current configuration.
    pub fn limits(mut self, limits: Limits) -> Self {
        self.config.max_objects = limits.max_objects;
        self.config.max_logical_heap = limits.max_logical_heap;
        self.config.max_stack_slots = limits.max_stack_slots;
        self.config.max_string_bytes = limits.max_string_bytes;
        self.config.max_snapshot_bytes = limits.max_snapshot_bytes;
        self
    }

    /// Install a host registry. Selected libraries register their own symbols.
    pub fn registry(mut self, registry: HostRegistry) -> Self {
        self.registry = registry;
        self
    }

    /// Select libraries to register and install together. No library grants host
    /// IO or OS authority; attach [`super::HostCapabilities`] separately.
    /// Restore automatically supplies library symbols; restrict the restoring
    /// host with [`super::Host::libraries`] when accepting untrusted snapshots.
    pub fn libraries(mut self, libraries: Libraries) -> Self {
        self.libraries = libraries;
        self
    }

    /// Set the output sink used by `print`.
    pub fn output(mut self, output: impl FnMut(&[u8]) + 'static) -> Self {
        self.capabilities = self.capabilities.output(output);
        self
    }

    /// Set the warnings sink, including Lua's continuation flag.
    pub fn warnings(mut self, warnings: impl FnMut(&[u8], bool) + 'static) -> Self {
        self.capabilities = self.capabilities.warnings(warnings);
        self
    }

    /// Set the entropy source used by unseeded `math.randomseed`.
    pub fn entropy(mut self, entropy: impl FnMut() -> i64 + 'static) -> Self {
        self.capabilities = self.capabilities.entropy(entropy);
        self
    }

    /// Replace all optional host capabilities together.
    pub fn capabilities(mut self, capabilities: super::HostCapabilities) -> Self {
        self.capabilities = capabilities;
        self
    }

    /// Supply resources used to rebind userdata on restore.
    pub fn host_env(mut self, env: super::HostEnv) -> Self {
        self.capabilities = self.capabilities.host_env(env);
        self
    }

    /// Install a host module searcher after the preload searcher.
    pub fn module_resolver(
        mut self,
        policy: super::ResolverPolicy,
        resolver: impl super::ModuleResolver,
    ) -> Self {
        self.capabilities = self.capabilities.module_resolver(policy, resolver);
        self
    }

    /// Build an idle runtime with an existing main thread. Resource failure
    /// is a Lua error; before a runtime exists its error value is nil.
    pub fn build(self) -> Result<Runtime> {
        Runtime::api_build(self)
    }
}
