use regex::Regex;
use std::sync::LazyLock;

pub(crate) static COLLECTION_NAME_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[a-zA-Z0-9_-]{1,64}$").unwrap());

pub(crate) static KEY_NAME_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[a-zA-Z0-9_.-]{1,256}$").unwrap());

pub(crate) static FIELD_NAME_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[a-zA-Z0-9_.-]{1,128}$").unwrap());
