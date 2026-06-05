pub struct SystemFields;

impl SystemFields {
    // Public API field names (returned to clients).
    pub const VERSION: &'static str = "_v";
    pub const KEY: &'static str = "_key";
    pub const SEQ: &'static str = "_seq";
    pub const CREATED_AT: &'static str = "_createdAt";
    pub const MODIFIED_AT: &'static str = "_modifiedAt";
    pub const EXPIRES_AT: &'static str = "_expiresAt";

    // Internal i8 token values stored as MsgPack negative FixInt keys on disk/WAL.
    pub const TOKEN_VERSION: i8 = -1;
    pub const TOKEN_KEY: i8 = -2;
    pub const TOKEN_SEQ: i8 = -3;
    pub const TOKEN_CREATED_AT: i8 = -4;
    pub const TOKEN_MODIFIED_AT: i8 = -5;
    pub const TOKEN_EXPIRES_AT: i8 = -6;

    // In-memory integer key strings — used as serde_json map keys in RAM.
    // These match the token values above, stored as their string representation.
    pub const IKEY_VERSION: &'static str = "-1";
    pub const IKEY_KEY: &'static str = "-2";
    pub const IKEY_SEQ: &'static str = "-3";
    pub const IKEY_CREATED_AT: &'static str = "-4";
    pub const IKEY_MODIFIED_AT: &'static str = "-5";
    pub const IKEY_EXPIRES_AT: &'static str = "-6";
}
