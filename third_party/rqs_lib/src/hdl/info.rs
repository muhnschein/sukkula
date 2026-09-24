use super::TextPayloadType;

/// Book-keeping for one file payload, in either direction.
#[derive(Debug)]
pub struct InternalFileInfo {
    pub payload_id: i64,
    pub bytes_transferred: i64,
    pub total_size: i64,
    /// Chunks sent so far (outbound only): the `index` of the next chunk.
    pub chunks: i32,
}

/// A file as the sender's introduction describes it. Every field is exactly
/// what the peer sent: untrusted, unchecked, never a path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IncomingFile {
    pub payload_id: i64,
    pub name: String,
    pub size: i64,
    pub mime_type: String,
}

/// A text as the sender's introduction describes it. Its content follows
/// as a byte payload once the transfer is accepted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IncomingText {
    pub payload_id: i64,
    pub kind: TextPayloadType,
    /// A preview the sender chose; untrusted.
    pub title: String,
    pub size: i64,
}

/// What a sender offers: files, or one text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Introduction {
    pub files: Vec<IncomingFile>,
    pub text: Option<IncomingText>,
}

/// Bytes of an accepted file, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileChunk {
    pub payload_id: i64,
    /// Where `body` starts in the file.
    pub offset: i64,
    pub body: Vec<u8>,
    /// The sender says the file is complete after `body`.
    pub last: bool,
}

/// A file to send: what the peer is told about it. The embedding
/// application reads the file and hands the bytes over chunk by chunk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutgoingFile {
    pub name: String,
    pub size: i64,
    pub mime_type: String,
}

/// A text to send.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutgoingText {
    pub kind: TextPayloadType,
    /// What the receiver's consent dialog previews.
    pub title: String,
    pub text: String,
}
