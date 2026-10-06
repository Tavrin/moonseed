use super::*;
/// Operation classification used by the single runtime boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EffectClass {
    /// Journal external observations, including errors.
    JournaledRead,
    /// Journal namespace/resource/output/process mutations exactly once.
    ExactlyOnceMutation,
}
/// A complete portable capability request. Its encoding is journal identity.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum CapabilityRequest {
    /// `CivilTime::zone_name`, journaled separately from the numeric offset.
    CivilTimeZoneName {
        /// UTC instant.
        utc_seconds: i64,
    },
    /// `Filesystem::probe_readable`.
    FilesystemProbeReadable {
        /// Exact `path` parameter.
        path: Vec<u8>,
    },
    /// `Filesystem::open`.
    FilesystemOpen {
        /// Exact `path` parameter.
        path: Vec<u8>,
        /// Exact `mode` parameter.
        mode: OpenMode,
    },
    /// `Filesystem::read_at`.
    FilesystemReadAt {
        /// Exact `id` parameter.
        id: ResourceId,
        /// Exact `offset` parameter.
        offset: u64,
        /// Exact `max` parameter.
        max: usize,
    },
    /// `Filesystem::write_at`.
    FilesystemWriteAt {
        /// Exact `id` parameter.
        id: ResourceId,
        /// Exact `offset` parameter.
        offset: u64,
        /// Exact `bytes` parameter.
        bytes: Vec<u8>,
    },
    /// `Filesystem::append`.
    FilesystemAppend {
        /// Exact `id` parameter.
        id: ResourceId,
        /// Exact `bytes` parameter.
        bytes: Vec<u8>,
    },
    /// `Filesystem::size`.
    FilesystemSize {
        /// Exact `id` parameter.
        id: ResourceId,
    },
    /// `Filesystem::flush`.
    FilesystemFlush {
        /// Exact `id` parameter.
        id: ResourceId,
    },
    /// `Filesystem::close`.
    FilesystemClose {
        /// Exact `id` parameter.
        id: ResourceId,
    },
    /// `Filesystem::remove`.
    FilesystemRemove {
        /// Exact `path` parameter.
        path: Vec<u8>,
    },
    /// `Filesystem::rename`.
    FilesystemRename {
        /// Exact `from` parameter.
        from: Vec<u8>,
        /// Exact `to` parameter.
        to: Vec<u8>,
    },
    /// `Filesystem::temp_file`.
    FilesystemTempFile,
    /// `Filesystem::temp_name`.
    FilesystemTempName,
    /// `Filesystem::read_file`.
    FilesystemReadFile {
        /// Exact `path` parameter.
        path: Vec<u8>,
        /// Exact `max` parameter.
        max: usize,
    },
    /// Bounded, handle-free positional source read.
    FilesystemReadFileRange {
        /// Exact byte path.
        path: Vec<u8>,
        /// Byte position within the source.
        offset: u64,
        /// Maximum bytes (at most 64 KiB).
        max: usize,
    },
    /// `Stdio::read_stdin`.
    StdioReadStdin {
        /// Exact `max` parameter.
        max: usize,
    },
    /// `Stdio::write_stdout`.
    StdioWriteStdout {
        /// Exact `bytes` parameter.
        bytes: Vec<u8>,
    },
    /// `Stdio::write_stderr`.
    StdioWriteStderr {
        /// Exact `bytes` parameter.
        bytes: Vec<u8>,
    },
    /// `Stdio::flush`.
    StdioFlush {
        /// Exact `stream` parameter.
        stream: Stream,
    },
    /// `Clock::now_seconds`.
    ClockNowSeconds,
    /// `Clock::cpu_seconds`.
    ClockCpuSeconds,
    /// `CivilTime::local_offset`.
    CivilTimeLocalOffset {
        /// Exact `utc_seconds` parameter.
        utc_seconds: i64,
    },
    /// `CivilTime::utc_seconds`.
    CivilTimeUtcSeconds {
        /// Exact `local_seconds` parameter.
        local_seconds: i64,
        /// Exact `isdst` parameter.
        isdst: Option<bool>,
    },
    /// `Environment::get`.
    EnvironmentGet {
        /// Exact `name` parameter.
        name: Vec<u8>,
    },
    /// `Process::shell_available`.
    ProcessShellAvailable,
    /// `Process::execute`.
    ProcessExecute {
        /// Exact `cmd` parameter.
        cmd: Vec<u8>,
    },
    /// `Process::popen`.
    ProcessPopen {
        /// Exact `cmd` parameter.
        cmd: Vec<u8>,
        /// Exact `mode` parameter.
        mode: PipeMode,
    },
    /// `Process::read_at`.
    ProcessReadAt {
        /// Exact `id` parameter.
        id: ResourceId,
        /// Exact `offset` parameter.
        offset: u64,
        /// Exact `max` parameter.
        max: usize,
    },
    /// `Process::write_at`.
    ProcessWriteAt {
        /// Exact `id` parameter.
        id: ResourceId,
        /// Exact `offset` parameter.
        offset: u64,
        /// Exact `bytes` parameter.
        bytes: Vec<u8>,
    },
    /// `Process::flush`.
    ProcessFlush {
        /// Exact `id` parameter.
        id: ResourceId,
    },
    /// `Process::close`.
    ProcessClose {
        /// Exact `id` parameter.
        id: ResourceId,
    },
}
/// Portable typed outcomes; encoded in the external journal, never as Lua results.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum CapabilityValue {
    /// No value.
    Unit,
    /// Boolean observation.
    Boolean(bool),
    /// Byte string.
    Bytes(Vec<u8>),
    /// Environment lookup; None is absence.
    OptionalBytes(Option<Vec<u8>>),
    /// Unsigned size, offset, or number of bytes written.
    Unsigned(u64),
    /// Signed wall/civil seconds.
    Integer(i64),
    /// CPU seconds (journal stores exact bits).
    Number(f64),
    /// Acquired backend resource.
    Resource(ResourceId),
    /// Local timezone observation.
    Offset(CivilOffset),
    /// Process termination.
    Status(ProcessStatus),
}
/// Runtime result of invoking a capability; waits use VM-issued keys.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum CapabilityPoll {
    /// Final outcome (including host failure).
    Ready(Result<CapabilityValue, HostIoError>),
    /// Resume with `Runtime::complete_capability`, then re-enter the same request.
    Waiting(crate::WaitKey),
}
impl CapabilityRequest {
    /// Semantic read/mutation classification (never inferred from open mode).
    pub fn class(&self) -> EffectClass {
        match self {
            Self::FilesystemProbeReadable { .. } => EffectClass::JournaledRead,
            Self::FilesystemOpen { .. } => EffectClass::ExactlyOnceMutation,
            Self::FilesystemReadAt { .. } => EffectClass::JournaledRead,
            Self::FilesystemWriteAt { .. } => EffectClass::ExactlyOnceMutation,
            Self::FilesystemAppend { .. } => EffectClass::ExactlyOnceMutation,
            Self::FilesystemSize { .. } => EffectClass::JournaledRead,
            Self::FilesystemFlush { .. } => EffectClass::ExactlyOnceMutation,
            Self::FilesystemClose { .. } => EffectClass::ExactlyOnceMutation,
            Self::FilesystemRemove { .. } => EffectClass::ExactlyOnceMutation,
            Self::FilesystemRename { .. } => EffectClass::ExactlyOnceMutation,
            Self::FilesystemTempFile => EffectClass::ExactlyOnceMutation,
            Self::FilesystemTempName => EffectClass::ExactlyOnceMutation,
            Self::FilesystemReadFile { .. } => EffectClass::JournaledRead,
            Self::FilesystemReadFileRange { .. } => EffectClass::JournaledRead,
            Self::StdioReadStdin { .. } => EffectClass::JournaledRead,
            Self::StdioWriteStdout { .. } => EffectClass::ExactlyOnceMutation,
            Self::StdioWriteStderr { .. } => EffectClass::ExactlyOnceMutation,
            Self::StdioFlush { .. } => EffectClass::ExactlyOnceMutation,
            Self::ClockNowSeconds => EffectClass::JournaledRead,
            Self::ClockCpuSeconds => EffectClass::JournaledRead,
            Self::CivilTimeZoneName { .. } => EffectClass::JournaledRead,
            Self::CivilTimeLocalOffset { .. } => EffectClass::JournaledRead,
            Self::CivilTimeUtcSeconds { .. } => EffectClass::JournaledRead,
            Self::EnvironmentGet { .. } => EffectClass::JournaledRead,
            Self::ProcessShellAvailable => EffectClass::JournaledRead,
            Self::ProcessExecute { .. } => EffectClass::ExactlyOnceMutation,
            Self::ProcessPopen { .. } => EffectClass::ExactlyOnceMutation,
            Self::ProcessReadAt { .. } => EffectClass::JournaledRead,
            Self::ProcessWriteAt { .. } => EffectClass::ExactlyOnceMutation,
            Self::ProcessFlush { .. } => EffectClass::ExactlyOnceMutation,
            Self::ProcessClose { .. } => EffectClass::ExactlyOnceMutation,
        }
    }
    /// Stable operation name shown through `Runtime::wait`.
    pub fn operation(&self) -> &'static str {
        match self {
            Self::FilesystemProbeReadable { .. } => "capability.filesystem.probe_readable",
            Self::FilesystemOpen { .. } => "capability.filesystem.open",
            Self::FilesystemReadAt { .. } => "capability.filesystem.read_at",
            Self::FilesystemWriteAt { .. } => "capability.filesystem.write_at",
            Self::FilesystemAppend { .. } => "capability.filesystem.append",
            Self::FilesystemSize { .. } => "capability.filesystem.size",
            Self::FilesystemFlush { .. } => "capability.filesystem.flush",
            Self::FilesystemClose { .. } => "capability.filesystem.close",
            Self::FilesystemRemove { .. } => "capability.filesystem.remove",
            Self::FilesystemRename { .. } => "capability.filesystem.rename",
            Self::FilesystemTempFile => "capability.filesystem.temp_file",
            Self::FilesystemTempName => "capability.filesystem.temp_name",
            Self::FilesystemReadFile { .. } => "capability.filesystem.read_file",
            Self::FilesystemReadFileRange { .. } => "capability.filesystem.read_file_range",
            Self::StdioReadStdin { .. } => "capability.stdio.read_stdin",
            Self::StdioWriteStdout { .. } => "capability.stdio.write_stdout",
            Self::StdioWriteStderr { .. } => "capability.stdio.write_stderr",
            Self::StdioFlush { .. } => "capability.stdio.flush",
            Self::ClockNowSeconds => "capability.clock.now_seconds",
            Self::ClockCpuSeconds => "capability.clock.cpu_seconds",
            Self::CivilTimeZoneName { .. } => "capability.civiltime.zone_name",
            Self::CivilTimeLocalOffset { .. } => "capability.civiltime.local_offset",
            Self::CivilTimeUtcSeconds { .. } => "capability.civiltime.utc_seconds",
            Self::EnvironmentGet { .. } => "capability.environment.get",
            Self::ProcessShellAvailable => "capability.process.shell_available",
            Self::ProcessExecute { .. } => "capability.process.execute",
            Self::ProcessPopen { .. } => "capability.process.popen",
            Self::ProcessReadAt { .. } => "capability.process.read_at",
            Self::ProcessWriteAt { .. } => "capability.process.write_at",
            Self::ProcessFlush { .. } => "capability.process.flush",
            Self::ProcessClose { .. } => "capability.process.close",
        }
    }
    /// Versioned, length-delimited, little-endian exact request identity.
    pub fn request_bytes(&self) -> Vec<u8> {
        let mut out = vec![1];
        match self {
            Self::FilesystemProbeReadable { path } => {
                out.push(0);
                put_bytes(&mut out, path);
            }
            Self::FilesystemOpen { path, mode } => {
                out.push(1);
                put_bytes(&mut out, path);
                out.push(
                    u8::from(mode.read)
                        | (u8::from(mode.write) << 1)
                        | (u8::from(mode.append) << 2)
                        | (u8::from(mode.create) << 3)
                        | (u8::from(mode.truncate) << 4)
                        | (u8::from(mode.binary) << 5),
                );
            }
            Self::FilesystemReadAt { id, offset, max } => {
                out.push(2);
                out.extend((id.0).to_le_bytes());
                out.extend(offset.to_le_bytes());
                out.extend((*max as u64).to_le_bytes());
            }
            Self::FilesystemWriteAt { id, offset, bytes } => {
                out.push(3);
                out.extend((id.0).to_le_bytes());
                out.extend(offset.to_le_bytes());
                put_bytes(&mut out, bytes);
            }
            Self::FilesystemAppend { id, bytes } => {
                out.push(4);
                out.extend((id.0).to_le_bytes());
                put_bytes(&mut out, bytes);
            }
            Self::FilesystemSize { id } => {
                out.push(5);
                out.extend((id.0).to_le_bytes());
            }
            Self::FilesystemFlush { id } => {
                out.push(6);
                out.extend((id.0).to_le_bytes());
            }
            Self::FilesystemClose { id } => {
                out.push(7);
                out.extend((id.0).to_le_bytes());
            }
            Self::FilesystemRemove { path } => {
                out.push(8);
                put_bytes(&mut out, path);
            }
            Self::FilesystemRename { from, to } => {
                out.push(9);
                put_bytes(&mut out, from);
                put_bytes(&mut out, to);
            }
            Self::FilesystemTempFile => {
                out.push(10);
            }
            Self::FilesystemTempName => {
                out.push(11);
            }
            Self::FilesystemReadFile { path, max } => {
                out.push(12);
                put_bytes(&mut out, path);
                out.extend((*max as u64).to_le_bytes());
            }
            Self::FilesystemReadFileRange { path, offset, max } => {
                out.push(30);
                put_bytes(&mut out, path);
                out.extend(offset.to_le_bytes());
                out.extend((*max as u64).to_le_bytes());
            }
            Self::StdioReadStdin { max } => {
                out.push(13);
                out.extend((*max as u64).to_le_bytes());
            }
            Self::StdioWriteStdout { bytes } => {
                out.push(14);
                put_bytes(&mut out, bytes);
            }
            Self::StdioWriteStderr { bytes } => {
                out.push(15);
                put_bytes(&mut out, bytes);
            }
            Self::StdioFlush { stream } => {
                out.push(16);
                out.push(*stream as u8);
            }
            Self::ClockNowSeconds => {
                out.push(17);
            }
            Self::ClockCpuSeconds => {
                out.push(18);
            }
            Self::CivilTimeZoneName { utc_seconds } => {
                out.push(29);
                out.extend(utc_seconds.to_le_bytes());
            }
            Self::CivilTimeLocalOffset { utc_seconds } => {
                out.push(19);
                out.extend(utc_seconds.to_le_bytes());
            }
            Self::CivilTimeUtcSeconds {
                local_seconds,
                isdst,
            } => {
                out.push(20);
                out.extend(local_seconds.to_le_bytes());
                out.push(match isdst {
                    None => 0,
                    Some(false) => 1,
                    Some(true) => 2,
                });
            }
            Self::EnvironmentGet { name } => {
                out.push(21);
                put_bytes(&mut out, name);
            }
            Self::ProcessShellAvailable => {
                out.push(22);
            }
            Self::ProcessExecute { cmd } => {
                out.push(23);
                put_bytes(&mut out, cmd);
            }
            Self::ProcessPopen { cmd, mode } => {
                out.push(24);
                put_bytes(&mut out, cmd);
                out.push(*mode as u8);
            }
            Self::ProcessReadAt { id, offset, max } => {
                out.push(25);
                out.extend((id.0).to_le_bytes());
                out.extend(offset.to_le_bytes());
                out.extend((*max as u64).to_le_bytes());
            }
            Self::ProcessWriteAt { id, offset, bytes } => {
                out.push(26);
                out.extend((id.0).to_le_bytes());
                out.extend(offset.to_le_bytes());
                put_bytes(&mut out, bytes);
            }
            Self::ProcessFlush { id } => {
                out.push(27);
                out.extend((id.0).to_le_bytes());
            }
            Self::ProcessClose { id } => {
                out.push(28);
                out.extend((id.0).to_le_bytes());
            }
        }
        out
    }
    pub(crate) fn invoke(&self, caps: &crate::HostCapabilities) -> Completion<CapabilityValue> {
        match self {
            Self::FilesystemProbeReadable { path } => caps.filesystem.as_ref().map_or_else(
                || Completion::Ready(Err(HostIoError::denied())),
                |c| c.probe_readable(path).map(CapabilityValue::Boolean),
            ),
            Self::FilesystemOpen { path, mode } => caps.filesystem.as_ref().map_or_else(
                || Completion::Ready(Err(HostIoError::denied())),
                |c| c.open(path, *mode).map(CapabilityValue::Resource),
            ),
            Self::FilesystemReadAt { id, offset, max } => caps.filesystem.as_ref().map_or_else(
                || Completion::Ready(Err(HostIoError::denied())),
                |c| c.read_at(*id, *offset, *max).map(CapabilityValue::Bytes),
            ),
            Self::FilesystemWriteAt { id, offset, bytes } => caps.filesystem.as_ref().map_or_else(
                || Completion::Ready(Err(HostIoError::denied())),
                |c| {
                    c.write_at(*id, *offset, bytes)
                        .map(|n| CapabilityValue::Unsigned(n as u64))
                },
            ),
            Self::FilesystemAppend { id, bytes } => caps.filesystem.as_ref().map_or_else(
                || Completion::Ready(Err(HostIoError::denied())),
                |c| c.append(*id, bytes).map(CapabilityValue::Unsigned),
            ),
            Self::FilesystemSize { id } => caps.filesystem.as_ref().map_or_else(
                || Completion::Ready(Err(HostIoError::denied())),
                |c| c.size(*id).map(CapabilityValue::Unsigned),
            ),
            Self::FilesystemFlush { id } => caps.filesystem.as_ref().map_or_else(
                || Completion::Ready(Err(HostIoError::denied())),
                |c| c.flush(*id).map(|()| CapabilityValue::Unit),
            ),
            Self::FilesystemClose { id } => caps.filesystem.as_ref().map_or_else(
                || Completion::Ready(Err(HostIoError::denied())),
                |c| c.close(*id).map(|()| CapabilityValue::Unit),
            ),
            Self::FilesystemRemove { path } => caps.filesystem.as_ref().map_or_else(
                || Completion::Ready(Err(HostIoError::denied())),
                |c| c.remove(path).map(|()| CapabilityValue::Unit),
            ),
            Self::FilesystemRename { from, to } => caps.filesystem.as_ref().map_or_else(
                || Completion::Ready(Err(HostIoError::denied())),
                |c| c.rename(from, to).map(|()| CapabilityValue::Unit),
            ),
            Self::FilesystemTempFile => caps.filesystem.as_ref().map_or_else(
                || Completion::Ready(Err(HostIoError::denied())),
                |c| c.temp_file().map(CapabilityValue::Resource),
            ),
            Self::FilesystemTempName => caps.filesystem.as_ref().map_or_else(
                || Completion::Ready(Err(HostIoError::denied())),
                |c| c.temp_name().map(CapabilityValue::Bytes),
            ),
            Self::FilesystemReadFile { path, max } => caps.filesystem.as_ref().map_or_else(
                || Completion::Ready(Err(HostIoError::denied())),
                |c| c.read_file(path, *max).map(CapabilityValue::Bytes),
            ),
            Self::FilesystemReadFileRange { path, offset, max } => {
                caps.filesystem.as_ref().map_or_else(
                    || Completion::Ready(Err(HostIoError::denied())),
                    |c| {
                        c.read_file_range(path, *offset, *max)
                            .map(CapabilityValue::Bytes)
                    },
                )
            }
            Self::StdioReadStdin { max } => caps.stdio.as_ref().map_or_else(
                || Completion::Ready(Err(HostIoError::denied())),
                |c| c.read_stdin(*max).map(CapabilityValue::Bytes),
            ),
            Self::StdioWriteStdout { bytes } => caps.stdio.as_ref().map_or_else(
                || Completion::Ready(Err(HostIoError::denied())),
                |c| {
                    c.write_stdout(bytes)
                        .map(|n| CapabilityValue::Unsigned(n as u64))
                },
            ),
            Self::StdioWriteStderr { bytes } => caps.stdio.as_ref().map_or_else(
                || Completion::Ready(Err(HostIoError::denied())),
                |c| {
                    c.write_stderr(bytes)
                        .map(|n| CapabilityValue::Unsigned(n as u64))
                },
            ),
            Self::StdioFlush { stream } => caps.stdio.as_ref().map_or_else(
                || Completion::Ready(Err(HostIoError::denied())),
                |c| c.flush(*stream).map(|()| CapabilityValue::Unit),
            ),
            Self::ClockNowSeconds => caps.clock.as_ref().map_or_else(
                || Completion::Ready(Err(HostIoError::denied())),
                |c| c.now_seconds().map(CapabilityValue::Integer),
            ),
            Self::ClockCpuSeconds => caps.clock.as_ref().map_or_else(
                || Completion::Ready(Err(HostIoError::denied())),
                |c| c.cpu_seconds().map(CapabilityValue::Number),
            ),
            Self::CivilTimeZoneName { utc_seconds } => caps.civil.as_ref().map_or_else(
                || Completion::Ready(Ok(CapabilityValue::Bytes(b"UTC".to_vec()))),
                |c| c.zone_name(*utc_seconds).map(CapabilityValue::Bytes),
            ),
            Self::CivilTimeLocalOffset { utc_seconds } => caps.civil.as_ref().map_or_else(
                || Completion::Ready(Err(HostIoError::denied())),
                |c| c.local_offset(*utc_seconds).map(CapabilityValue::Offset),
            ),
            Self::CivilTimeUtcSeconds {
                local_seconds,
                isdst,
            } => caps.civil.as_ref().map_or_else(
                || Completion::Ready(Err(HostIoError::denied())),
                |c| {
                    c.utc_seconds(*local_seconds, *isdst)
                        .map(CapabilityValue::Integer)
                },
            ),
            Self::EnvironmentGet { name } => caps.environment.as_ref().map_or_else(
                || Completion::Ready(Err(HostIoError::denied())),
                |c| c.get(name).map(CapabilityValue::OptionalBytes),
            ),
            Self::ProcessShellAvailable => caps.process.as_ref().map_or_else(
                || Completion::Ready(Err(HostIoError::denied())),
                |c| c.shell_available().map(CapabilityValue::Boolean),
            ),
            Self::ProcessExecute { cmd } => caps.process.as_ref().map_or_else(
                || Completion::Ready(Err(HostIoError::denied())),
                |c| c.execute(cmd).map(CapabilityValue::Status),
            ),
            Self::ProcessPopen { cmd, mode } => caps.process.as_ref().map_or_else(
                || Completion::Ready(Err(HostIoError::denied())),
                |c| c.popen(cmd, *mode).map(CapabilityValue::Resource),
            ),
            Self::ProcessReadAt { id, offset, max } => caps.process.as_ref().map_or_else(
                || Completion::Ready(Err(HostIoError::denied())),
                |c| c.read_at(*id, *offset, *max).map(CapabilityValue::Bytes),
            ),
            Self::ProcessWriteAt { id, offset, bytes } => caps.process.as_ref().map_or_else(
                || Completion::Ready(Err(HostIoError::denied())),
                |c| {
                    c.write_at(*id, *offset, bytes)
                        .map(|n| CapabilityValue::Unsigned(n as u64))
                },
            ),
            Self::ProcessFlush { id } => caps.process.as_ref().map_or_else(
                || Completion::Ready(Err(HostIoError::denied())),
                |c| c.flush(*id).map(|()| CapabilityValue::Unit),
            ),
            Self::ProcessClose { id } => caps.process.as_ref().map_or_else(
                || Completion::Ready(Err(HostIoError::denied())),
                |c| c.close(*id).map(CapabilityValue::Status),
            ),
        }
    }
    pub(crate) fn accepts(&self, value: &CapabilityValue) -> bool {
        match self {
            Self::FilesystemProbeReadable { .. } => matches!(value, CapabilityValue::Boolean(_)),
            Self::FilesystemOpen { .. } => matches!(value, CapabilityValue::Resource(_)),
            Self::FilesystemReadAt { .. } => matches!(value, CapabilityValue::Bytes(_)),
            Self::FilesystemWriteAt { .. } => matches!(value, CapabilityValue::Unsigned(_)),
            Self::FilesystemAppend { .. } => matches!(value, CapabilityValue::Unsigned(_)),
            Self::FilesystemSize { .. } => matches!(value, CapabilityValue::Unsigned(_)),
            Self::FilesystemFlush { .. } => matches!(value, CapabilityValue::Unit),
            Self::FilesystemClose { .. } => matches!(value, CapabilityValue::Unit),
            Self::FilesystemRemove { .. } => matches!(value, CapabilityValue::Unit),
            Self::FilesystemRename { .. } => matches!(value, CapabilityValue::Unit),
            Self::FilesystemTempFile => matches!(value, CapabilityValue::Resource(_)),
            Self::FilesystemTempName => matches!(value, CapabilityValue::Bytes(_)),
            Self::FilesystemReadFile { .. } => matches!(value, CapabilityValue::Bytes(_)),
            Self::FilesystemReadFileRange { .. } => matches!(value, CapabilityValue::Bytes(_)),
            Self::StdioReadStdin { .. } => matches!(value, CapabilityValue::Bytes(_)),
            Self::StdioWriteStdout { .. } => matches!(value, CapabilityValue::Unsigned(_)),
            Self::StdioWriteStderr { .. } => matches!(value, CapabilityValue::Unsigned(_)),
            Self::StdioFlush { .. } => matches!(value, CapabilityValue::Unit),
            Self::ClockNowSeconds => matches!(value, CapabilityValue::Integer(_)),
            Self::ClockCpuSeconds => matches!(value, CapabilityValue::Number(_)),
            Self::CivilTimeZoneName { .. } => {
                matches!(value, CapabilityValue::Bytes(b) if b.len() <= 4096)
            }
            Self::CivilTimeLocalOffset { .. } => matches!(value, CapabilityValue::Offset(_)),
            Self::CivilTimeUtcSeconds { .. } => matches!(value, CapabilityValue::Integer(_)),
            Self::EnvironmentGet { .. } => matches!(value, CapabilityValue::OptionalBytes(_)),
            Self::ProcessShellAvailable => matches!(value, CapabilityValue::Boolean(_)),
            Self::ProcessExecute { .. } => matches!(value, CapabilityValue::Status(_)),
            Self::ProcessPopen { .. } => matches!(value, CapabilityValue::Resource(_)),
            Self::ProcessReadAt { .. } => matches!(value, CapabilityValue::Bytes(_)),
            Self::ProcessWriteAt { .. } => matches!(value, CapabilityValue::Unsigned(_)),
            Self::ProcessFlush { .. } => matches!(value, CapabilityValue::Unit),
            Self::ProcessClose { .. } => matches!(value, CapabilityValue::Status(_)),
        }
    }
}
pub(crate) fn put_bytes(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend((bytes.len() as u64).to_le_bytes());
    out.extend(bytes);
}
pub(crate) fn encode(result: &Result<CapabilityValue, HostIoError>) -> Vec<u8> {
    let mut out = vec![1];
    match result {
        Err(e) => {
            out.push(0);
            out.push(e.kind as u8);
            out.extend(e.code.to_le_bytes());
            put_bytes(&mut out, &e.message);
        }
        Ok(v) => {
            out.push(1);
            match v {
                CapabilityValue::Unit => out.push(0),
                CapabilityValue::Boolean(b) => {
                    out.push(1);
                    out.push(u8::from(*b));
                }
                CapabilityValue::Bytes(b) => {
                    out.push(2);
                    put_bytes(&mut out, b);
                }
                CapabilityValue::OptionalBytes(b) => {
                    out.push(3);
                    out.push(u8::from(b.is_some()));
                    if let Some(b) = b {
                        put_bytes(&mut out, b);
                    }
                }
                CapabilityValue::Unsigned(n) => {
                    out.push(4);
                    out.extend(n.to_le_bytes());
                }
                CapabilityValue::Integer(n) => {
                    out.push(5);
                    out.extend(n.to_le_bytes());
                }
                CapabilityValue::Number(n) => {
                    out.push(6);
                    out.extend(n.to_bits().to_le_bytes());
                }
                CapabilityValue::Resource(n) => {
                    out.push(7);
                    out.extend(n.0.to_le_bytes());
                }
                CapabilityValue::Offset(n) => {
                    out.push(8);
                    out.extend(n.seconds.to_le_bytes());
                    out.push(u8::from(n.isdst));
                }
                CapabilityValue::Status(n) => {
                    out.push(9);
                    let (tag, n) = match n {
                        ProcessStatus::Exit(n) => (0, n),
                        ProcessStatus::Signal(n) => (1, n),
                    };
                    out.push(tag);
                    out.extend(n.to_le_bytes());
                }
            }
        }
    }
    out
}
pub(crate) fn decode(
    mut bytes: &[u8],
) -> Result<Result<CapabilityValue, HostIoError>, crate::VmError> {
    use crate::VmError;
    fn take<const N: usize>(bytes: &mut &[u8]) -> Result<[u8; N], VmError> {
        let head = bytes.get(..N).ok_or(VmError::Corrupt)?;
        let result = head.try_into().map_err(|_| VmError::Corrupt)?;
        *bytes = &bytes[N..];
        Ok(result)
    }
    fn blob(bytes: &mut &[u8]) -> Result<Vec<u8>, VmError> {
        let n = usize::try_from(u64::from_le_bytes(take(bytes)?)).map_err(|_| VmError::Corrupt)?;
        let b = bytes.get(..n).ok_or(VmError::Corrupt)?.to_vec();
        *bytes = &bytes[n..];
        Ok(b)
    }
    fn bit(bytes: &mut &[u8]) -> Result<bool, VmError> {
        match take::<1>(bytes)?[0] {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(VmError::Corrupt),
        }
    }
    if take::<1>(&mut bytes)? != [1] {
        return Err(VmError::Corrupt);
    }
    let result = match take::<1>(&mut bytes)?[0] {
        0 => {
            let kind = match take::<1>(&mut bytes)?[0] {
                0 => HostIoErrorKind::NotFound,
                1 => HostIoErrorKind::PermissionDenied,
                2 => HostIoErrorKind::AlreadyExists,
                3 => HostIoErrorKind::IsDirectory,
                4 => HostIoErrorKind::InvalidInput,
                5 => HostIoErrorKind::Unsupported,
                6 => HostIoErrorKind::Other,
                _ => return Err(VmError::Corrupt),
            };
            Err(HostIoError {
                kind,
                code: i32::from_le_bytes(take(&mut bytes)?),
                message: blob(&mut bytes)?,
            })
        }
        1 => Ok(match take::<1>(&mut bytes)?[0] {
            0 => CapabilityValue::Unit,
            1 => CapabilityValue::Boolean(bit(&mut bytes)?),
            2 => CapabilityValue::Bytes(blob(&mut bytes)?),
            3 => CapabilityValue::OptionalBytes(if bit(&mut bytes)? {
                Some(blob(&mut bytes)?)
            } else {
                None
            }),
            4 => CapabilityValue::Unsigned(u64::from_le_bytes(take(&mut bytes)?)),
            5 => CapabilityValue::Integer(i64::from_le_bytes(take(&mut bytes)?)),
            6 => CapabilityValue::Number(f64::from_bits(u64::from_le_bytes(take(&mut bytes)?))),
            7 => CapabilityValue::Resource(ResourceId(u64::from_le_bytes(take(&mut bytes)?))),
            8 => CapabilityValue::Offset(CivilOffset {
                seconds: i32::from_le_bytes(take(&mut bytes)?),
                isdst: bit(&mut bytes)?,
            }),
            9 => {
                let tag = take::<1>(&mut bytes)?[0];
                let n = i32::from_le_bytes(take(&mut bytes)?);
                CapabilityValue::Status(match tag {
                    0 => ProcessStatus::Exit(n),
                    1 => ProcessStatus::Signal(n),
                    _ => return Err(VmError::Corrupt),
                })
            }
            _ => return Err(VmError::Corrupt),
        }),
        _ => return Err(VmError::Corrupt),
    };
    if !bytes.is_empty() {
        return Err(VmError::Corrupt);
    }
    Ok(result)
}

impl CapabilityRequest {
    pub(crate) fn from_bytes(mut b: &[u8]) -> Result<Self, crate::VmError> {
        use crate::VmError;
        fn take<const N: usize>(b: &mut &[u8]) -> Result<[u8; N], VmError> {
            let n = b
                .get(..N)
                .ok_or(VmError::Corrupt)?
                .try_into()
                .map_err(|_| VmError::Corrupt)?;
            *b = &b[N..];
            Ok(n)
        }
        fn blob(b: &mut &[u8]) -> Result<Vec<u8>, VmError> {
            let n = usize::try_from(u64::from_le_bytes(take(b)?)).map_err(|_| VmError::Corrupt)?;
            let v = b.get(..n).ok_or(VmError::Corrupt)?.to_vec();
            *b = &b[n..];
            Ok(v)
        }
        if take::<1>(&mut b)? != [1] {
            return Err(VmError::Corrupt);
        }
        let req = match take::<1>(&mut b)?[0] {
            0 => Self::FilesystemProbeReadable {
                path: blob(&mut b)?,
            },
            1 => Self::FilesystemOpen {
                path: blob(&mut b)?,
                mode: {
                    let bits = take::<1>(&mut b)?[0];
                    if bits > 63 {
                        return Err(VmError::Corrupt);
                    }
                    OpenMode {
                        read: bits & 1 != 0,
                        write: bits & 2 != 0,
                        append: bits & 4 != 0,
                        create: bits & 8 != 0,
                        truncate: bits & 16 != 0,
                        binary: bits & 32 != 0,
                    }
                },
            },
            2 => Self::FilesystemReadAt {
                id: ResourceId(u64::from_le_bytes(take(&mut b)?)),
                offset: u64::from_le_bytes(take(&mut b)?),
                max: usize::try_from(u64::from_le_bytes(take(&mut b)?))
                    .map_err(|_| VmError::Corrupt)?,
            },
            3 => Self::FilesystemWriteAt {
                id: ResourceId(u64::from_le_bytes(take(&mut b)?)),
                offset: u64::from_le_bytes(take(&mut b)?),
                bytes: blob(&mut b)?,
            },
            4 => Self::FilesystemAppend {
                id: ResourceId(u64::from_le_bytes(take(&mut b)?)),
                bytes: blob(&mut b)?,
            },
            5 => Self::FilesystemSize {
                id: ResourceId(u64::from_le_bytes(take(&mut b)?)),
            },
            6 => Self::FilesystemFlush {
                id: ResourceId(u64::from_le_bytes(take(&mut b)?)),
            },
            7 => Self::FilesystemClose {
                id: ResourceId(u64::from_le_bytes(take(&mut b)?)),
            },
            8 => Self::FilesystemRemove {
                path: blob(&mut b)?,
            },
            9 => Self::FilesystemRename {
                from: blob(&mut b)?,
                to: blob(&mut b)?,
            },
            10 => Self::FilesystemTempFile,
            11 => Self::FilesystemTempName,
            12 => Self::FilesystemReadFile {
                path: blob(&mut b)?,
                max: usize::try_from(u64::from_le_bytes(take(&mut b)?))
                    .map_err(|_| VmError::Corrupt)?,
            },
            30 => Self::FilesystemReadFileRange {
                path: blob(&mut b)?,
                offset: u64::from_le_bytes(take(&mut b)?),
                max: usize::try_from(u64::from_le_bytes(take(&mut b)?))
                    .map_err(|_| VmError::Corrupt)?,
            },
            13 => Self::StdioReadStdin {
                max: usize::try_from(u64::from_le_bytes(take(&mut b)?))
                    .map_err(|_| VmError::Corrupt)?,
            },
            14 => Self::StdioWriteStdout {
                bytes: blob(&mut b)?,
            },
            15 => Self::StdioWriteStderr {
                bytes: blob(&mut b)?,
            },
            16 => Self::StdioFlush {
                stream: match take::<1>(&mut b)?[0] {
                    0 => Stream::Stdin,
                    1 => Stream::Stdout,
                    2 => Stream::Stderr,
                    _ => return Err(VmError::Corrupt),
                },
            },
            17 => Self::ClockNowSeconds,
            18 => Self::ClockCpuSeconds,
            19 => Self::CivilTimeLocalOffset {
                utc_seconds: i64::from_le_bytes(take(&mut b)?),
            },
            20 => Self::CivilTimeUtcSeconds {
                local_seconds: i64::from_le_bytes(take(&mut b)?),
                isdst: match take::<1>(&mut b)?[0] {
                    0 => None,
                    1 => Some(false),
                    2 => Some(true),
                    _ => return Err(VmError::Corrupt),
                },
            },
            21 => Self::EnvironmentGet {
                name: blob(&mut b)?,
            },
            22 => Self::ProcessShellAvailable,
            23 => Self::ProcessExecute { cmd: blob(&mut b)? },
            24 => Self::ProcessPopen {
                cmd: blob(&mut b)?,
                mode: match take::<1>(&mut b)?[0] {
                    0 => PipeMode::Read,
                    1 => PipeMode::Write,
                    _ => return Err(VmError::Corrupt),
                },
            },
            25 => Self::ProcessReadAt {
                id: ResourceId(u64::from_le_bytes(take(&mut b)?)),
                offset: u64::from_le_bytes(take(&mut b)?),
                max: usize::try_from(u64::from_le_bytes(take(&mut b)?))
                    .map_err(|_| VmError::Corrupt)?,
            },
            26 => Self::ProcessWriteAt {
                id: ResourceId(u64::from_le_bytes(take(&mut b)?)),
                offset: u64::from_le_bytes(take(&mut b)?),
                bytes: blob(&mut b)?,
            },
            27 => Self::ProcessFlush {
                id: ResourceId(u64::from_le_bytes(take(&mut b)?)),
            },
            29 => Self::CivilTimeZoneName {
                utc_seconds: i64::from_le_bytes(take(&mut b)?),
            },
            28 => Self::ProcessClose {
                id: ResourceId(u64::from_le_bytes(take(&mut b)?)),
            },
            _ => return Err(VmError::Corrupt),
        };
        if !b.is_empty() {
            return Err(VmError::Corrupt);
        }
        Ok(req)
    }
    pub(crate) fn bounded(&self) -> bool {
        match self {
            Self::FilesystemProbeReadable { path, .. } => {
                let _ = path;
                true
            }
            Self::FilesystemOpen { path, mode, .. } => {
                let _ = path;
                mode.valid()
            }
            Self::FilesystemReadAt {
                id, offset, max, ..
            } => {
                let _ = id;
                let _ = offset;
                *max <= 64 * 1024
            }
            Self::FilesystemWriteAt {
                id, offset, bytes, ..
            } => {
                let _ = id;
                let _ = offset;
                bytes.len() <= 64 * 1024
            }
            Self::FilesystemAppend { id, bytes, .. } => {
                let _ = id;
                bytes.len() <= 64 * 1024
            }
            Self::FilesystemSize { id, .. } => {
                let _ = id;
                true
            }
            Self::FilesystemFlush { id, .. } => {
                let _ = id;
                true
            }
            Self::FilesystemClose { id, .. } => {
                let _ = id;
                true
            }
            Self::FilesystemRemove { path, .. } => {
                let _ = path;
                true
            }
            Self::FilesystemRename { from, to, .. } => {
                let _ = from;
                let _ = to;
                true
            }
            Self::FilesystemTempFile => true,
            Self::FilesystemTempName => true,
            Self::FilesystemReadFile { path, max, .. } => {
                let _ = path;
                *max <= 64 * 1024
            }
            Self::FilesystemReadFileRange { max, .. } => *max <= 64 * 1024,
            Self::StdioReadStdin { max, .. } => *max <= 64 * 1024,
            Self::StdioWriteStdout { bytes, .. } => bytes.len() <= 64 * 1024,
            Self::StdioWriteStderr { bytes, .. } => bytes.len() <= 64 * 1024,
            Self::StdioFlush { stream, .. } => {
                let _ = stream;
                true
            }
            Self::ClockNowSeconds => true,
            Self::ClockCpuSeconds => true,
            Self::CivilTimeZoneName { .. } => true,
            Self::CivilTimeLocalOffset { utc_seconds, .. } => {
                let _ = utc_seconds;
                true
            }
            Self::CivilTimeUtcSeconds {
                local_seconds,
                isdst,
                ..
            } => {
                let _ = local_seconds;
                let _ = isdst;
                true
            }
            Self::EnvironmentGet { name, .. } => {
                let _ = name;
                true
            }
            Self::ProcessShellAvailable => true,
            Self::ProcessExecute { cmd, .. } => {
                let _ = cmd;
                true
            }
            Self::ProcessPopen { cmd, mode, .. } => {
                let _ = cmd;
                let _ = mode;
                true
            }
            Self::ProcessReadAt {
                id, offset, max, ..
            } => {
                let _ = id;
                let _ = offset;
                *max <= 64 * 1024
            }
            Self::ProcessWriteAt {
                id, offset, bytes, ..
            } => {
                let _ = id;
                let _ = offset;
                bytes.len() <= 64 * 1024
            }
            Self::ProcessFlush { id, .. } => {
                let _ = id;
                true
            }
            Self::ProcessClose { id, .. } => {
                let _ = id;
                true
            }
        }
    }
    pub(crate) fn valid_result(&self, r: &Result<CapabilityValue, HostIoError>) -> bool {
        let Ok(v) = r else {
            return true;
        };
        if !self.accepts(v) {
            return false;
        }
        match (self, v) {
            (Self::FilesystemReadAt { max, .. }, CapabilityValue::Bytes(bytes)) => {
                bytes.len() <= *max
            }
            (Self::FilesystemWriteAt { bytes, .. }, CapabilityValue::Unsigned(n)) => {
                *n <= bytes.len() as u64
            }
            (
                Self::FilesystemReadFile { max, .. } | Self::FilesystemReadFileRange { max, .. },
                CapabilityValue::Bytes(bytes),
            ) => bytes.len() <= *max,
            (Self::StdioReadStdin { max, .. }, CapabilityValue::Bytes(bytes)) => {
                bytes.len() <= *max
            }
            (Self::StdioWriteStdout { bytes, .. }, CapabilityValue::Unsigned(n)) => {
                *n <= bytes.len() as u64
            }
            (Self::StdioWriteStderr { bytes, .. }, CapabilityValue::Unsigned(n)) => {
                *n <= bytes.len() as u64
            }
            (Self::ProcessReadAt { max, .. }, CapabilityValue::Bytes(bytes)) => bytes.len() <= *max,
            (Self::ProcessWriteAt { bytes, .. }, CapabilityValue::Unsigned(n)) => {
                *n <= bytes.len() as u64
            }
            (_, CapabilityValue::Resource(id)) => id.0 != 0,
            (_, CapabilityValue::Number(n)) => n.is_finite() && *n >= 0.0,
            _ => true,
        }
    }
}

impl CapabilityRequest {
    pub(crate) fn encoded_len(&self) -> usize {
        match self {
            Self::FilesystemProbeReadable { path, .. } => 10usize.saturating_add(path.len()),
            Self::FilesystemOpen { path, .. } => 11usize.saturating_add(path.len()),
            Self::FilesystemReadAt { .. } => 26usize,
            Self::FilesystemWriteAt { bytes, .. } => 26usize.saturating_add(bytes.len()),
            Self::FilesystemAppend { bytes, .. } => 18usize.saturating_add(bytes.len()),
            Self::FilesystemSize { .. } => 10usize,
            Self::FilesystemFlush { .. } => 10usize,
            Self::FilesystemClose { .. } => 10usize,
            Self::FilesystemRemove { path, .. } => 10usize.saturating_add(path.len()),
            Self::FilesystemRename { from, to, .. } => {
                18usize.saturating_add(from.len()).saturating_add(to.len())
            }
            Self::FilesystemTempFile => 2usize,
            Self::FilesystemTempName => 2usize,
            Self::FilesystemReadFile { path, .. } => 18usize.saturating_add(path.len()),
            Self::FilesystemReadFileRange { path, .. } => 26usize.saturating_add(path.len()),
            Self::StdioReadStdin { .. } => 10usize,
            Self::StdioWriteStdout { bytes, .. } => 10usize.saturating_add(bytes.len()),
            Self::StdioWriteStderr { bytes, .. } => 10usize.saturating_add(bytes.len()),
            Self::StdioFlush { .. } => 3usize,
            Self::ClockNowSeconds => 2usize,
            Self::ClockCpuSeconds => 2usize,
            Self::CivilTimeZoneName { .. } => 10usize,
            Self::CivilTimeLocalOffset { .. } => 10usize,
            Self::CivilTimeUtcSeconds { .. } => 11usize,
            Self::EnvironmentGet { name, .. } => 10usize.saturating_add(name.len()),
            Self::ProcessShellAvailable => 2usize,
            Self::ProcessExecute { cmd, .. } => 10usize.saturating_add(cmd.len()),
            Self::ProcessPopen { cmd, .. } => 11usize.saturating_add(cmd.len()),
            Self::ProcessReadAt { .. } => 26usize,
            Self::ProcessWriteAt { bytes, .. } => 26usize.saturating_add(bytes.len()),
            Self::ProcessFlush { .. } => 10usize,
            Self::ProcessClose { .. } => 10usize,
        }
    }
}
