//! Hand-authored prost declarations matching fixtures/index.proto at the pinned
//! upstream revision. No protoc or build-time network access is required.
#[derive(Clone, PartialEq, prost::Message)]
pub struct Index {
    #[prost(string, tag = "1")]
    pub name: String,
    #[prost(string, tag = "2")]
    pub badge_label: String,
    #[prost(string, tag = "3")]
    pub signing_key: String,
    #[prost(message, optional, tag = "4")]
    pub contact: Option<Contact>,
    #[prost(oneof = "Extensions", tags = "101, 102")]
    pub extensions: Option<Extensions>,
}
#[derive(Clone, PartialEq, prost::Oneof)]
pub enum Extensions {
    #[prost(message, tag = "101")]
    Embedded(ExtensionList),
    #[prost(string, tag = "102")]
    Url(String),
}
#[derive(Clone, PartialEq, prost::Message)]
pub struct Contact {
    #[prost(string, tag = "1")]
    pub website: String,
    #[prost(string, optional, tag = "2")]
    pub discord: Option<String>,
}
#[derive(Clone, PartialEq, prost::Message)]
pub struct ExtensionList {
    #[prost(message, repeated, tag = "1")]
    pub extensions: Vec<Extension>,
}
#[derive(Clone, PartialEq, prost::Message)]
pub struct Extension {
    #[prost(string, tag = "1")]
    pub name: String,
    #[prost(string, tag = "2")]
    pub package_name: String,
    #[prost(message, optional, tag = "3")]
    pub resources: Option<Resources>,
    #[prost(string, tag = "4")]
    pub extension_lib: String,
    #[prost(int64, tag = "5")]
    pub version_code: i64,
    #[prost(string, tag = "6")]
    pub version_name: String,
    #[prost(int32, tag = "7")]
    pub content_warning: i32,
    #[prost(message, repeated, tag = "8")]
    pub sources: Vec<Source>,
}
#[derive(Clone, PartialEq, prost::Message)]
pub struct Resources {
    #[prost(string, tag = "1")]
    pub apk_url: String,
    #[prost(string, tag = "2")]
    pub icon_url: String,
    #[prost(string, tag = "501")]
    pub jar_url: String,
}
#[derive(Clone, PartialEq, prost::Message)]
pub struct Source {
    #[prost(int64, tag = "1")]
    pub id: i64,
    #[prost(string, tag = "2")]
    pub name: String,
    #[prost(string, tag = "3")]
    pub language: String,
    #[prost(string, tag = "4")]
    pub home_url: String,
    #[prost(string, repeated, tag = "5")]
    pub mirror_urls: Vec<String>,
    #[prost(string, optional, tag = "7")]
    pub message: Option<String>,
}

// Enforce collection limits before prost allocates repeated messages. A 32 MiB
// document consisting of millions of empty records must not amplify into GiB.
pub(crate) fn guard(bytes: &[u8], is_index: bool) -> crate::Result<()> {
    fn varint(bytes: &[u8], cursor: &mut usize) -> crate::Result<u64> {
        let mut value = 0_u64;
        for shift in (0..70).step_by(7) {
            let byte = *bytes
                .get(*cursor)
                .ok_or_else(|| crate::invalid("truncated protobuf"))?;
            *cursor += 1;
            if shift == 63 && byte > 1 {
                return Err(crate::invalid("protobuf varint overflow"));
            }
            value |= u64::from(byte & 127) << shift;
            if byte < 128 {
                return Ok(value);
            }
        }
        Err(crate::invalid("protobuf varint overflow"))
    }
    fn scan(bytes: &[u8], role: u8, records: &mut usize, fields: &mut usize) -> crate::Result<()> {
        let mut cursor = 0;
        while cursor < bytes.len() {
            *fields += 1;
            if *fields > crate::MAX_RECORDS * 16 {
                return Err(crate::CatalogueError::Limit);
            }
            let key = varint(bytes, &mut cursor)?;
            let tag = key >> 3;
            if tag == 0 {
                return Err(crate::invalid("protobuf tag zero"));
            }
            let length = match key & 7 {
                0 => {
                    varint(bytes, &mut cursor)?;
                    continue;
                }
                1 => 8,
                2 => usize::try_from(varint(bytes, &mut cursor)?).map_err(crate::invalid)?,
                5 => 4,
                _ => return Err(crate::invalid("unsupported protobuf wire type")),
            };
            let end = cursor
                .checked_add(length)
                .filter(|end| *end <= bytes.len())
                .ok_or_else(|| crate::invalid("truncated protobuf field"))?;
            if key & 7 == 2 {
                let child_role = match (role, tag) {
                    (0, 101) => Some(1),
                    (1, 1) => Some(2),
                    (2, 8) => Some(3),
                    _ => None,
                };
                if let Some(child_role) = child_role {
                    if child_role >= 2 {
                        *records += 1;
                    }
                    if *records > crate::MAX_RECORDS {
                        return Err(crate::CatalogueError::Limit);
                    }
                    scan(&bytes[cursor..end], child_role, records, fields)?;
                }
            }
            cursor = end;
        }
        Ok(())
    }
    scan(bytes, if is_index { 0 } else { 1 }, &mut 0, &mut 0)
}
