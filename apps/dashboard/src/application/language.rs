#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UserLanguage {
    En,
    De,
}

impl UserLanguage {
    pub fn from_raw(raw: Option<&str>) -> Self {
        let candidate = raw
            .unwrap_or("en")
            .split(',')
            .next()
            .unwrap_or("en")
            .split(['-', '_'])
            .next()
            .unwrap_or("en")
            .to_lowercase();
        match candidate.as_str() {
            "de" => UserLanguage::De,
            _ => UserLanguage::En,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            UserLanguage::En => "en",
            UserLanguage::De => "de",
        }
    }
}
