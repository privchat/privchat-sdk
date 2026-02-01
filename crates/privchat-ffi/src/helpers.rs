//! Helper utilities for FFI layer

use std::sync::Arc;

/// Unwrap an Arc or clone it if there are multiple references
/// 
/// This is useful for the builder pattern where we want to mutate
/// but also support multiple references.
pub fn unwrap_or_clone_arc<T: Clone>(arc: Arc<T>) -> T {
    Arc::try_unwrap(arc).unwrap_or_else(|arc| (*arc).clone())
}

/// Get the tokio runtime handle
/// 
/// This ensures we have a runtime for async operations
pub fn get_runtime() -> &'static tokio::runtime::Runtime {
    use std::sync::OnceLock;
    static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("Failed to create tokio runtime")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_unwrap_or_clone_arc() {
        let arc = Arc::new(42);
        let value = unwrap_or_clone_arc(arc);
        assert_eq!(value, 42);
        
        let arc = Arc::new(String::from("test"));
        let arc2 = arc.clone();
        let value = unwrap_or_clone_arc(arc);
        assert_eq!(value, "test");
        assert_eq!(*arc2, "test");
    }
}
