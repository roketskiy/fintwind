//! Source-weighted LRU for parsed/rendered transcript data.

use std::collections::HashMap;
use uuid::Uuid;

pub struct RenderCache<T> {
    entries: HashMap<Uuid, Entry<T>>,
    bytes: usize,
    clock: u64,
    frame: u64,
}

struct Entry<T> {
    value: T,
    bytes: usize,
    touched: u64,
    frame: u64,
}

impl<T> Default for RenderCache<T> {
    fn default() -> Self {
        Self {
            entries: HashMap::new(),
            bytes: 0,
            clock: 0,
            frame: 0,
        }
    }
}

impl<T> RenderCache<T> {
    pub fn begin_frame(&mut self) {
        self.frame = self.frame.wrapping_add(1);
    }

    pub fn get_or_insert_with(
        &mut self,
        id: Uuid,
        bytes: usize,
        build: impl FnOnce() -> T,
    ) -> &mut T {
        const MAX_SOURCE_BYTES: usize = 512 * 1024;
        const MAX_ENTRIES: usize = 128;
        self.clock = self.clock.wrapping_add(1);
        let entry = self.entries.entry(id).or_insert_with(|| Entry {
            value: build(),
            bytes: 0,
            touched: 0,
            frame: 0,
        });
        self.bytes = self.bytes.saturating_sub(entry.bytes).saturating_add(bytes);
        entry.bytes = bytes;
        entry.touched = self.clock;
        entry.frame = self.frame;
        // Keep this and the preceding window frame to avoid reparsing visible
        // rows every frame. The budget applies to history outside that working
        // set; a large visible document is never discarded while rendering.
        while self.entries.len() > 1
            && (self.bytes > MAX_SOURCE_BYTES || self.entries.len() > MAX_ENTRIES)
        {
            let oldest = self
                .entries
                .iter()
                .filter(|(key, entry)| **key != id && entry.frame.saturating_add(1) < self.frame)
                .min_by_key(|(_, entry)| entry.touched)
                .map(|(key, _)| *key);
            if let Some(oldest) = oldest {
                self.remove(&oldest);
            } else {
                break;
            }
        }
        &mut self
            .entries
            .get_mut(&id)
            .expect("current entry is retained")
            .value
    }

    pub fn contains_key(&self, id: &Uuid) -> bool {
        self.entries.contains_key(id)
    }

    pub fn remove(&mut self, id: &Uuid) {
        if let Some(entry) = self.entries.remove(id) {
            self.bytes = self.bytes.saturating_sub(entry.bytes);
        }
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.bytes = 0;
    }
}
