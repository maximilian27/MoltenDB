pub struct SystemFields;

impl SystemFields {
    // Public API field names (returned to clients).
    pub const VERSION: &'static str = "_v";
    pub const KEY: &'static str = "_key";
    pub const SEQ: &'static str = "_seq";
    pub const CREATED_AT: &'static str = "_createdAt";
    pub const MODIFIED_AT: &'static str = "_modifiedAt";
    pub const EXPIRES_AT: &'static str = "_expiresAt";

    // Internal i8 token values stored as MsgPack negative FixInt keys.
    // These are the string representations used as serde_json map keys
    // before the custom MsgPack encoder converts them to single bytes.
    pub const TOKEN_VERSION: i8 = -1;
    pub const TOKEN_KEY: i8 = -2;
    pub const TOKEN_SEQ: i8 = -3;
    pub const TOKEN_CREATED_AT: i8 = -4;
    pub const TOKEN_MODIFIED_AT: i8 = -5;
    pub const TOKEN_EXPIRES_AT: i8 = -6;
}
