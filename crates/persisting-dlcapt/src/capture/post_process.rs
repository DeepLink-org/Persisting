use crate::capture::event::CaptureEvent;
use anyhow::Result;

pub trait PostProcessor: Send + Sync {
    fn name(&self) -> &'static str;
    fn process(&self, _event: &mut CaptureEvent) -> Result<()> {
        Ok(())
    }
}

pub struct PostProcessorChain {
    processors: Vec<Box<dyn PostProcessor>>,
}

impl PostProcessorChain {
    pub fn new(processors: Vec<Box<dyn PostProcessor>>) -> Self {
        Self { processors }
    }

    pub fn empty() -> Self {
        Self {
            processors: Vec::new(),
        }
    }

    pub fn apply(&self, event: &mut CaptureEvent) {
        for processor in &self.processors {
            if let Err(err) = processor.process(event) {
                tracing::warn!(
                    processor = processor.name(),
                    error = %err,
                    "post-processor failed; skipping"
                );
            }
        }
    }
}
