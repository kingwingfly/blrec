/// Strips redundant FLV container headers on reconnections so that
/// downstream ffmpeg can continue decoding the concatenated stream.
pub struct FlvStripper {
    pub first_connection: bool,
}

impl FlvStripper {
    pub fn new() -> Self {
        Self {
            first_connection: true,
        }
    }

    /// Process a chunk of FLV data.
    /// On the first connection data passes through unchanged.
    /// On reconnects the FLV header (9 bytes) + first PreviousTagSize0 (4 bytes)
    /// and any leading script-data tags (type 0x12) are stripped,
    /// preserving audio/video tags so the downstream decoder re-initialises.
    pub fn process<'a>(&mut self, data: &'a [u8]) -> &'a [u8] {
        if self.first_connection {
            return data;
        }

        // Skip FLV header (9) + first PreviousTagSize (4)
        let mut offset = 13usize;

        // Skip script-data tags (type 0x12) until we hit a media tag
        while offset + 11 <= data.len() {
            let tag_type = data[offset] & 0x1F;
            if tag_type == 0x08 || tag_type == 0x09 {
                // First audio or video tag — stop stripping
                break;
            }
            if offset + 15 > data.len() {
                break;
            }
            let data_size = ((data[offset + 1] as usize) << 16)
                | ((data[offset + 2] as usize) << 8)
                | (data[offset + 3] as usize);
            // 11-byte tag header + data + 4-byte PreviousTagSize
            offset += 11 + data_size + 4;
        }

        if offset >= data.len() {
            return &[];
        }
        &data[offset..]
    }

    /// Call after a reconnection so the next `process` call strips headers.
    pub fn mark_reconnect(&mut self) {
        self.first_connection = false;
    }
}

/// Wraps `FlvStripper` and optionally drops audio or video tags from the FLV
/// byte stream.  The returned slice is valid until the next `process` call.
pub struct FlvFilter {
    stripper: FlvStripper,
    keep_audio: bool,
    keep_video: bool,
    buf: Vec<u8>,
}

impl FlvFilter {
    pub fn new(keep_audio: bool, keep_video: bool) -> Self {
        Self {
            stripper: FlvStripper::new(),
            keep_audio,
            keep_video,
            buf: Vec::with_capacity(65536),
        }
    }

    /// Process a chunk of FLV data, returning the (possibly filtered) bytes
    /// that should be written to output.
    pub fn process<'a>(&'a mut self, data: &'a [u8]) -> &'a [u8] {
        let stripped = self.stripper.process(data);
        if self.keep_audio && self.keep_video {
            return stripped; // no filtering needed, zero-copy
        }

        self.buf.clear();

        let mut offset = 0;
        while offset + 11 <= stripped.len() {
            let tag_type = stripped[offset] & 0x1F;
            let data_size = ((stripped[offset + 1] as usize) << 16)
                | ((stripped[offset + 2] as usize) << 8)
                | (stripped[offset + 3] as usize);
            let total_tag_len = 11 + data_size + 4; // header + data + prev_tag_size

            let keep = match tag_type {
                0x08 => self.keep_audio,
                0x09 => self.keep_video,
                _ => true, // script data etc. — keep
            };

            if keep && offset + total_tag_len <= stripped.len() {
                self.buf
                    .extend_from_slice(&stripped[offset..offset + total_tag_len]);
            }

            offset += total_tag_len.min(stripped.len().saturating_sub(offset));
        }

        // Any trailing bytes that don't form a complete tag — keep them
        // (they're likely a partial tag that will be completed in the next chunk)
        if offset < stripped.len() {
            self.buf.extend_from_slice(&stripped[offset..]);
        }

        &self.buf
    }

    pub fn mark_reconnect(&mut self) {
        self.stripper.mark_reconnect();
    }
}
