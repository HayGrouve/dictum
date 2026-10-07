use std::path::Path;
use std::sync::Once;

use anyhow::{Context, Result};
use ort::session::Session;
use ort::session::builder::GraphOptimizationLevel;

use crate::Device;

static INIT: Once = Once::new();

pub(crate) fn init_runtime() {
    INIT.call_once(|| {
        let _ = ort::init().with_name("dictum").commit();
    });
}

pub(crate) enum Source<'a> {
    File(&'a Path),
    Memory(&'a [u8]),
}

/// Builds an ONNX Runtime session tuned for low-latency, single-stream inference.
pub(crate) fn build(source: Source<'_>, threads: usize, device: Device) -> Result<Session> {
    init_runtime();
    let built: ort::Result<Session> = (|| {
        let mut builder = Session::builder()?
            .with_optimization_level(GraphOptimizationLevel::Level3)?
            .with_inter_threads(1)?;
        if threads > 0 {
            builder = builder.with_intra_threads(threads)?;
        }
        if device == Device::Gpu {
            builder = with_gpu(builder)?;
        }
        match source {
            Source::File(path) => builder.commit_from_file(path),
            Source::Memory(bytes) => builder.commit_from_memory(bytes),
        }
    })();
    built.with_context(|| match source {
        Source::File(path) => format!("failed to load ONNX model {}", path.display()),
        Source::Memory(_) => "failed to load embedded ONNX model".to_string(),
    })
}

#[cfg(windows)]
fn with_gpu(
    builder: ort::session::builder::SessionBuilder,
) -> ort::Result<ort::session::builder::SessionBuilder> {
    // DirectML requires sequential execution without memory patterns.
    Ok(builder
        .with_parallel_execution(false)?
        .with_memory_pattern(false)?
        .with_execution_providers([ort::ep::DirectML::default().build()])?)
}

#[cfg(target_os = "macos")]
fn with_gpu(
    builder: ort::session::builder::SessionBuilder,
) -> ort::Result<ort::session::builder::SessionBuilder> {
    Ok(builder.with_execution_providers([ort::ep::CoreML::default().build()])?)
}

#[cfg(not(any(windows, target_os = "macos")))]
fn with_gpu(
    builder: ort::session::builder::SessionBuilder,
) -> ort::Result<ort::session::builder::SessionBuilder> {
    log::warn!("GPU inference is not supported on this platform; using the CPU");
    Ok(builder)
}
