use crate::sample::{
    CpuSample, DiskSample, GpuSample, MemorySample, NetworkSample, ProcessSample,
};

/// Unified event type carried over the [`crate::DataBus`].
///
/// Every [`crate::Collector::Sample`] should have a `From` impl into
/// `MetricEvent`, so the daemon can publish samples without subsystem-specific
/// glue: `bus.publish(sample)`.
#[derive(Debug, Clone)]
pub enum MetricEvent {
    Cpu(CpuSample),
    Memory(MemorySample),
    Network(NetworkSample),
    Disk(DiskSample),
    Process(ProcessSample),
    Gpu(GpuSample),
}

impl From<CpuSample> for MetricEvent {
    fn from(s: CpuSample) -> Self {
        Self::Cpu(s)
    }
}

impl From<MemorySample> for MetricEvent {
    fn from(s: MemorySample) -> Self {
        Self::Memory(s)
    }
}

impl From<NetworkSample> for MetricEvent {
    fn from(s: NetworkSample) -> Self {
        Self::Network(s)
    }
}

impl From<DiskSample> for MetricEvent {
    fn from(s: DiskSample) -> Self {
        Self::Disk(s)
    }
}

impl From<ProcessSample> for MetricEvent {
    fn from(s: ProcessSample) -> Self {
        Self::Process(s)
    }
}

impl From<GpuSample> for MetricEvent {
    fn from(s: GpuSample) -> Self {
        Self::Gpu(s)
    }
}

impl MetricEvent {
    /// Static label for tracing / logging — useful when fan-out consumers want
    /// to filter without pattern-matching the whole enum.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Cpu(_) => "cpu",
            Self::Memory(_) => "memory",
            Self::Network(_) => "network",
            Self::Disk(_) => "disk",
            Self::Process(_) => "process",
            Self::Gpu(_) => "gpu",
        }
    }
}
