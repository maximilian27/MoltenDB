// ─── log_commands.rs ──────────────────────────────────────────────────────────
// Typed constants for all LogEntry command strings.
//
// Tokens -1 through -9 are reserved for system field keys (see system_fields.rs).
// Log command tokens start at -10.
// ─────────────────────────────────────────────────────────────────────────────

pub struct LogCommand;

impl LogCommand {
    // ── Human-readable command strings (used in public API / debug output) ──
    pub const INSERT: &'static str = "INSERT";
    pub const DELETE: &'static str = "DELETE";
    pub const DROP: &'static str = "DROP";
    pub const INDEX: &'static str = "INDEX";
    pub const SCHEMA: &'static str = "SCHEMA";
    pub const ENC: &'static str = "ENC";
    pub const TX_BEGIN: &'static str = "TX_BEGIN";
    pub const TX_COMMIT: &'static str = "TX_COMMIT";

    // ── Internal i8 token values stored as MsgPack negative FixInt in WAL ──
    // Tokens -1 through -9 are reserved for system fields.
    pub const TOKEN_INSERT: i8 = -10;
    pub const TOKEN_DELETE: i8 = -11;
    pub const TOKEN_DROP: i8 = -12;
    pub const TOKEN_INDEX: i8 = -13;
    pub const TOKEN_SCHEMA: i8 = -14;
    pub const TOKEN_ENC: i8 = -15;
    pub const TOKEN_TX_BEGIN: i8 = -16;
    pub const TOKEN_TX_COMMIT: i8 = -17;

    // ── In-memory integer key strings — the cmd field value stored in RAM ──
    pub const IKEY_INSERT: &'static str = "-10";
    pub const IKEY_DELETE: &'static str = "-11";
    pub const IKEY_DROP: &'static str = "-12";
    pub const IKEY_INDEX: &'static str = "-13";
    pub const IKEY_SCHEMA: &'static str = "-14";
    pub const IKEY_ENC: &'static str = "-15";
    pub const IKEY_TX_BEGIN: &'static str = "-16";
    pub const IKEY_TX_COMMIT: &'static str = "-17";

    /// Convert a human-readable command string to its in-memory IKEY token.
    /// Returns the original string unchanged if it is not a known command.
    pub fn to_ikey(cmd: &str) -> &'static str {
        match cmd {
            Self::INSERT => Self::IKEY_INSERT,
            Self::DELETE => Self::IKEY_DELETE,
            Self::DROP => Self::IKEY_DROP,
            Self::INDEX => Self::IKEY_INDEX,
            Self::SCHEMA => Self::IKEY_SCHEMA,
            Self::ENC => Self::IKEY_ENC,
            Self::TX_BEGIN => Self::IKEY_TX_BEGIN,
            Self::TX_COMMIT => Self::IKEY_TX_COMMIT,
            _ => "",
        }
    }

    /// Expand an in-memory IKEY token back to its human-readable command string.
    /// Returns `None` if the token is not recognised.
    pub fn from_ikey(ikey: &str) -> Option<&'static str> {
        match ikey {
            Self::IKEY_INSERT => Some(Self::INSERT),
            Self::IKEY_DELETE => Some(Self::DELETE),
            Self::IKEY_DROP => Some(Self::DROP),
            Self::IKEY_INDEX => Some(Self::INDEX),
            Self::IKEY_SCHEMA => Some(Self::SCHEMA),
            Self::IKEY_ENC => Some(Self::ENC),
            Self::IKEY_TX_BEGIN => Some(Self::TX_BEGIN),
            Self::IKEY_TX_COMMIT => Some(Self::TX_COMMIT),
            _ => None,
        }
    }
}
