//! Stateful filter to strip injected system tags from WebSocket deltas.

use regex::Regex;

const SUPPRESS_TAGS: &[&str] = &["system", "reminder", "context", "hidden", "instructions", "note"];
const TAG_GUARD: usize = 20;

pub struct InjectionFilter {
    suppressing: Option<String>,
    buf: String,
}

impl InjectionFilter {
    pub fn new() -> Self {
        Self {
            suppressing: None,
            buf: String::with_capacity(1024),
        }
    }

    pub fn feed(&mut self, delta: &str) -> String {
        self.buf.push_str(delta);
        let mut out = String::new();

        while !self.buf.is_empty() {
            if let Some(suppressing_tag) = &self.suppressing {
                let close_tag = format!("</{}>", suppressing_tag);
                if let Some(idx) = self.buf.to_lowercase().find(&close_tag.to_lowercase()) {
                    let _ = self.buf.drain(..idx + close_tag.len());
                    self.suppressing = None;
                } else {
                    let guard = close_tag.len().saturating_sub(1);
                    if self.buf.len() > guard {
                        self.buf = self.buf.split_off(self.buf.len() - guard);
                    }
                    break;
                }
            } else {
                let mut earliest = self.buf.len();
                let mut earliest_tag = None;
                for tag in SUPPRESS_TAGS {
                    let pattern = format!(r"(?i)<{}[\s>/]", tag);
                    if let Some(m) = Regex::new(&pattern).unwrap().find(&self.buf) {
                        if m.start() < earliest {
                            earliest = m.start();
                            earliest_tag = Some(tag);
                        }
                    }
                }

                if let Some(tag) = earliest_tag {
                    out.push_str(&self.buf[..earliest]);
                    self.buf = self.buf.split_off(earliest);
                    self.suppressing = Some(tag.to_string());
                    if let Some(m) = Regex::new(&format!(r"(?i)<{}[^>]*>", tag))
                        .unwrap()
                        .find(&self.buf)
                    {
                        let _ = self.buf.drain(..m.end());
                    } else {
                        break;
                    }
                } else {
                    if self.buf.len() > TAG_GUARD {
                        let safe_len = self
                            .buf
                            .char_indices()
                            .map(|(idx, _)| idx)
                            .filter(|idx| self.buf.len() - idx > TAG_GUARD)
                            .last()
                            .unwrap_or(0);
                        let safe = self.buf[..safe_len].to_string();
                        out.push_str(&safe);
                        self.buf = self.buf.split_off(safe_len);
                    } else if !self.buf.contains('<') {
                        out.push_str(&self.buf);
                        self.buf.clear();
                    }
                    break;
                }
            }
        }

        out
    }

    pub fn flush(&mut self) -> String {
        if self.suppressing.is_some() {
            self.suppressing = None;
            self.buf.clear();
            return String::new();
        }
        std::mem::take(&mut self.buf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_filter_single_tag() {
        let mut f = InjectionFilter::new();
        let input = "Hello <system>secret</system> world";
        assert_eq!(f.feed(input), "Hello  world");
        assert_eq!(f.flush(), "");
    }

    #[test]
    fn test_filter_multiple_tags() {
        let mut f = InjectionFilter::new();
        let input = "A <reminder>hidden</reminder> B <system>more</system> C";
        assert_eq!(f.feed(input), "A  B  C");
        assert_eq!(f.flush(), "");
    }
}
