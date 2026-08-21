use std::collections::HashMap;

#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct Properties {
    raw: HashMap<String, String>,
}

impl Properties {
    pub fn new() -> Self {
        Self {
            raw: HashMap::new(),
        }
    }

    pub fn insert(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.raw.insert(key.into(), value.into());
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.raw.get(key).map(|s| s.as_str())
    }

    pub fn vendor(&self) -> Option<&str> {
        self.get("openslide.vendor")
    }

    pub fn mpp(&self) -> Option<(f64, f64)> {
        let x = self.get("openslide.mpp-x")?.parse().ok()?;
        let y = self.get("openslide.mpp-y")?.parse().ok()?;
        Some((x, y))
    }

    pub fn objective_power(&self) -> Option<f64> {
        self.get("openslide.objective-power")?.parse().ok()
    }

    pub fn background_color(&self) -> [u8; 3] {
        self.get("openslide.background-color")
            .and_then(|s| {
                let s = s.trim_start_matches('#');
                if s.len() == 6 {
                    let r = u8::from_str_radix(&s[0..2], 16).ok()?;
                    let g = u8::from_str_radix(&s[2..4], 16).ok()?;
                    let b = u8::from_str_radix(&s[4..6], 16).ok()?;
                    Some([r, g, b])
                } else {
                    None
                }
            })
            .unwrap_or([255, 255, 255])
    }

    pub fn quickhash1(&self) -> Option<&str> {
        self.get("openslide.quickhash-1")
    }

    pub fn names(&self) -> Vec<&str> {
        let mut names: Vec<&str> = self.raw.keys().map(|s| s.as_str()).collect();
        names.sort();
        names
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.raw.iter().map(|(k, v)| (k.as_str(), v.as_str()))
    }

    pub fn len(&self) -> usize {
        self.raw.len()
    }

    pub fn is_empty(&self) -> bool {
        self.raw.is_empty()
    }
}

#[cfg(test)]
mod tests;
