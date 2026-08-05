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
