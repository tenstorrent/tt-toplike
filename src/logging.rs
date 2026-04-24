//! Logging infrastructure with message buffering
//!
//! This module provides a custom log implementation that captures log messages
//! into a thread-safe buffer, making them accessible to both TUI and GUI for display.

use log::{Level, LevelFilter, Metadata, Record};
use std::collections::VecDeque;
use std::sync::{atomic::{AtomicBool, Ordering}, Arc, Mutex, Once};

/// Maximum number of log messages to keep in the buffer
const MAX_LOG_MESSAGES: usize = 100;

/// Global flag to disable stderr output (for TUI mode)
static STDERR_DISABLED: AtomicBool = AtomicBool::new(false);

/// A single log message with metadata
#[derive(Clone, Debug)]
pub struct LogMessage {
    /// Log level (Error, Warn, Info, Debug, Trace)
    pub level: Level,
    /// Message content
    pub message: String,
    /// Timestamp (formatted string)
    pub timestamp: String,
}

/// Thread-safe message buffer
type MessageBuffer = Arc<Mutex<VecDeque<LogMessage>>>;

/// Global message buffer (initialized once)
static mut MESSAGE_BUFFER: Option<MessageBuffer> = None;
static INIT: Once = Once::new();

/// Custom logger that writes to both stderr and the message buffer
struct BufferedLogger {
    buffer: MessageBuffer,
    level: LevelFilter,
}

impl log::Log for BufferedLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= self.level
    }

    fn log(&self, record: &Record) {
        if self.enabled(record.metadata()) {
            let timestamp = chrono::Local::now().format("%H:%M:%S").to_string();

            // Format the log message
            let message = format!("{}", record.args());

            // Write to stderr only if not disabled (TUI mode disables this)
            if !STDERR_DISABLED.load(Ordering::Relaxed) {
                eprintln!("[{}] {} - {}", timestamp, record.level(), message);
            }

            // Add to buffer
            if let Ok(mut buffer) = self.buffer.lock() {
                buffer.push_back(LogMessage {
                    level: record.level(),
                    message: message.clone(),
                    timestamp: timestamp.clone(),
                });

                // Keep buffer size limited
                while buffer.len() > MAX_LOG_MESSAGES {
                    buffer.pop_front();
                }
            }
        }
    }

    fn flush(&self) {}
}

/// Initialize logging with message buffering
///
/// This sets up the global logger to write to both stderr and a message buffer.
/// Call this once at application startup.
///
/// # Arguments
///
/// * `level` - Log level filter (Info, Debug, Trace, etc.)
///
/// # Example
///
/// ```rust,no_run
/// use log::LevelFilter;
/// use tt_toplike::logging::init_logging_with_buffer;
///
/// init_logging_with_buffer(LevelFilter::Info);
/// log::info!("Application started");
/// ```
pub fn init_logging_with_buffer(level: LevelFilter) {
    INIT.call_once(|| {
        // Create the global message buffer
        let buffer = Arc::new(Mutex::new(VecDeque::new()));

        // Store in global static
        unsafe {
            MESSAGE_BUFFER = Some(buffer.clone());
        }

        // Create and install the logger
        let logger = BufferedLogger {
            buffer,
            level,
        };

        if let Err(e) = log::set_boxed_logger(Box::new(logger)) {
            eprintln!("Failed to set logger: {}", e);
            return;
        }

        log::set_max_level(level);
    });
}

/// Get a copy of all log messages in the buffer
///
/// Returns a vector of recent log messages (up to MAX_LOG_MESSAGES).
/// Messages are ordered from oldest to newest.
///
/// # Example
///
/// ```rust,no_run
/// use tt_toplike::logging::get_log_messages;
///
/// for msg in get_log_messages() {
///     println!("[{}] {}: {}", msg.timestamp, msg.level, msg.message);
/// }
/// ```
pub fn get_log_messages() -> Vec<LogMessage> {
    unsafe {
        if let Some(ref buffer) = MESSAGE_BUFFER {
            if let Ok(buffer) = buffer.lock() {
                return buffer.iter().cloned().collect();
            }
        }
    }
    Vec::new()
}

/// Get the N most recent log messages
///
/// # Arguments
///
/// * `count` - Maximum number of messages to return
///
/// # Example
///
/// ```rust,no_run
/// use tt_toplike::logging::get_recent_log_messages;
///
/// // Get last 10 messages
/// for msg in get_recent_log_messages(10) {
///     println!("{}", msg.message);
/// }
/// ```
pub fn get_recent_log_messages(count: usize) -> Vec<LogMessage> {
    unsafe {
        if let Some(ref buffer) = MESSAGE_BUFFER {
            if let Ok(buffer) = buffer.lock() {
                let start = buffer.len().saturating_sub(count);
                return buffer.iter().skip(start).cloned().collect();
            }
        }
    }
    Vec::new()
}

/// Clear all log messages from the buffer
///
/// This is useful for testing or when you want to reset the message history.
pub fn clear_log_messages() {
    unsafe {
        if let Some(ref buffer) = MESSAGE_BUFFER {
            if let Ok(mut buffer) = buffer.lock() {
                buffer.clear();
            }
        }
    }
}

/// Get the current buffer size
///
/// Returns the number of messages currently stored in the buffer.
pub fn get_log_message_count() -> usize {
    unsafe {
        if let Some(ref buffer) = MESSAGE_BUFFER {
            if let Ok(buffer) = buffer.lock() {
                return buffer.len();
            }
        }
    }
    0
}

/// Disable stderr output (for TUI mode)
///
/// Call this when entering TUI mode to prevent log messages from
/// corrupting the terminal display. Messages will still be buffered.
///
/// # Example
///
/// ```rust,no_run
/// use tt_toplike::logging::disable_stderr;
///
/// // Before entering TUI alternate screen
/// disable_stderr();
/// // ... TUI code
/// ```
pub fn disable_stderr() {
    STDERR_DISABLED.store(true, Ordering::Relaxed);
}

/// Enable stderr output (default)
///
/// Call this when exiting TUI mode to restore normal logging behavior.
///
/// # Example
///
/// ```rust,no_run
/// use tt_toplike::logging::enable_stderr;
///
/// // After exiting TUI alternate screen
/// enable_stderr();
/// ```
pub fn enable_stderr() {
    STDERR_DISABLED.store(false, Ordering::Relaxed);
}

#[cfg(test)]
mod tests {
    use super::*;
    use log::LevelFilter;

    #[test]
    fn test_log_buffer() {
        init_logging_with_buffer(LevelFilter::Debug);

        log::info!("Test message 1");
        log::warn!("Test message 2");
        log::error!("Test message 3");

        let messages = get_log_messages();
        assert!(messages.len() >= 3);

        // Check that messages are captured
        assert!(messages.iter().any(|m| m.message.contains("Test message")));
    }

    #[test]
    fn test_buffer_limit() {
        clear_log_messages();

        // Add more than MAX_LOG_MESSAGES
        for i in 0..MAX_LOG_MESSAGES + 50 {
            log::info!("Message {}", i);
        }

        let count = get_log_message_count();
        assert!(count <= MAX_LOG_MESSAGES);
    }

    #[test]
    fn test_recent_messages() {
        init_logging_with_buffer(LevelFilter::Debug);

        // Use unique markers to avoid races with parallel tests.
        let tag = format!("TESTRECENT-{:?}", std::thread::current().id());
        log::info!("{}-A", tag);
        log::info!("{}-B", tag);
        log::info!("{}-C", tag);

        // Fetch a window large enough to include our messages.
        let recent = get_recent_log_messages(200);
        let a_pos = recent.iter().rposition(|m| m.message.contains(&format!("{}-A", tag)));
        let b_pos = recent.iter().rposition(|m| m.message.contains(&format!("{}-B", tag)));
        let c_pos = recent.iter().rposition(|m| m.message.contains(&format!("{}-C", tag)));

        assert!(a_pos.is_some() && b_pos.is_some() && c_pos.is_some(),
                "Expected all three tagged messages to appear in the log");
        assert!(a_pos.unwrap() < b_pos.unwrap() && b_pos.unwrap() < c_pos.unwrap(),
                "Expected messages in A < B < C order");
    }
}
