use super::*;

/// Error-preserving wrapper around wbcore's chat aggregator for non-stream
/// Responses, Messages and compaction handlers. The core aggregator intentionally
/// ignores in-band errors; this wrapper never converts those errors into success.
#[derive(Default)]
pub struct CheckedAggregator {
    inner: wbcore::aggregate::Aggregator,
    error: Option<TranslationError>,
}
impl CheckedAggregator {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn push_line(&mut self, line: &str) -> Result<(), TranslationError> {
        if let Some(e) = &self.error {
            return Err(e.clone());
        }
        let data = wbcore::sse::strip_data_prefix(line);
        if let Ok(chunk) = serde_json::from_str::<Value>(data) {
            return self.push_chunk(&chunk);
        }
        self.inner.push_line(line);
        Ok(())
    }
    pub fn push_chunk(&mut self, chunk: &Value) -> Result<(), TranslationError> {
        if let Some(e) = &self.error {
            return Err(e.clone());
        }
        if let Some(e) = upstream_error(chunk) {
            self.error = Some(e.clone());
            return Err(e);
        }
        self.inner.push_chunk(chunk);
        Ok(())
    }
    pub fn abort(&mut self, message: &str) -> TranslationError {
        self.error
            .get_or_insert_with(|| {
                TranslationError::upstream(format!("upstream stream aborted: {message}"))
            })
            .clone()
    }
    /// Consumes state and either returns the original error or the final chat
    /// object. Even if a caller ignored a push error, finish still fails.
    pub fn finish(self, model: &str, response_id: Option<&str>) -> Result<Value, TranslationError> {
        if let Some(error) = self.error {
            return Err(error);
        }
        Ok(self.inner.finish(model, response_id).obj)
    }
}
