use std::fmt;

/// Finite limits shared by composition and connections.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bounds {
    /// Maximum frame or response body size, from 8 KiB to 8 MiB.
    pub message_bytes: usize,
    /// Maximum serialised catalogue size, at most 64 MiB.
    pub catalogue_bytes: usize,
    /// Maximum size of each input or output schema, no greater than a message.
    pub schema_bytes: usize,
    /// Maximum operations in a catalogue, from 1 to 4096.
    pub tools: usize,
    /// Maximum catalogue pages, from 1 to 1024.
    pub pages: usize,
    /// Maximum concurrent requests, from 1 to 32.
    pub in_flight: usize,
    /// Maximum queued requests, from 0 to 1024.
    pub waiting: usize,
    /// Maximum incoming requests and notifications over a connection's life, from 1 to 16384.
    pub notifications: usize,
}

impl Default for Bounds {
    fn default() -> Self {
        Self {
            message_bytes: 1024 * 1024,
            catalogue_bytes: 8 * 1024 * 1024,
            schema_bytes: 128 * 1024,
            tools: 512,
            pages: 32,
            in_flight: 4,
            waiting: 16,
            notifications: 4096,
        }
    }
}

impl Bounds {
    /// Check individual limits and the combined 64 MiB buffering budget.
    pub fn validate(&self) -> Result<(), InvalidBounds> {
        if !(8192..=8 * 1024 * 1024).contains(&self.message_bytes)
            || !(1..=64 * 1024 * 1024).contains(&self.catalogue_bytes)
            || self.schema_bytes == 0
            || self.schema_bytes > self.message_bytes
            || !(1..=4096).contains(&self.tools)
            || !(1..=1024).contains(&self.pages)
            || !(1..=32).contains(&self.in_flight)
            || self.waiting > 1024
            || !(1..=16384).contains(&self.notifications)
            || self.message_bytes.saturating_mul(
                self.in_flight
                    .saturating_add(self.waiting)
                    .saturating_add(2),
            ) > 64 * 1024 * 1024
        {
            return Err(InvalidBounds);
        }
        Ok(())
    }
}

/// Limits outside the supported finite ranges or buffering budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidBounds;

impl fmt::Display for InvalidBounds {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("bounds exceed the supported ranges or the 64 MiB buffering budget")
    }
}

impl std::error::Error for InvalidBounds {}
