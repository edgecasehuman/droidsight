use serde::Serialize;

const MAX_CRASHES: usize = 200;
const MAX_RAW_BYTES: usize = 64 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Crash {
    #[serde(rename = "type")]
    pub kind: String,
    pub timestamp: String,
    pub process: String,
    pub summary: String,
    pub raw: String,
}

pub fn parse(text: &str) -> Vec<Crash> {
    let lines: Vec<_> = text.lines().collect();
    let mut crashes = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        if line.contains("FATAL EXCEPTION") && line.contains("AndroidRuntime:") {
            let mut raw: Vec<&str> = Vec::new();
            let mut process = String::new();
            let mut summary = String::new();
            let timestamp = timestamp(line);
            let mut j = i;
            while j < lines.len()
                && j < i + 200
                && raw.iter().map(|line| line.len() + 1).sum::<usize>() < MAX_RAW_BYTES
            {
                if j > i
                    && (lines[j].contains("FATAL EXCEPTION")
                        || lines[j].contains("ANR in ")
                        || lines[j].contains("am_anr"))
                {
                    break;
                }
                if !lines[j].contains("AndroidRuntime:") {
                    j += 1;
                    continue;
                }
                let content = lines[j]
                    .split_once("AndroidRuntime:")
                    .map_or("", |(_, v)| v.trim());
                raw.push(lines[j]);
                if process.is_empty() {
                    if let Some(rest) = content.strip_prefix("Process: ") {
                        process = rest.split(',').next().unwrap_or("").trim().to_owned();
                    }
                }
                if summary.is_empty()
                    && !content.contains("FATAL EXCEPTION")
                    && (content.contains("Exception") || content.contains("Error"))
                {
                    summary = content.to_owned();
                }
                j += 1;
            }
            crashes.push(Crash {
                kind: "java".into(),
                timestamp,
                process,
                summary: if summary.is_empty() {
                    "FATAL EXCEPTION".into()
                } else {
                    summary
                },
                raw: raw.join("\n"),
            });
            if crashes.len() >= MAX_CRASHES {
                break;
            }
            i = j;
            continue;
        }
        if let Some((_, tail)) = line.split_once("ANR in ") {
            let process = tail
                .split(|c: char| c.is_whitespace() || c == '(')
                .next()
                .unwrap_or("")
                .to_owned();
            crashes.push(Crash {
                kind: "anr".into(),
                timestamp: timestamp(line),
                process,
                summary: tail.trim().to_owned(),
                raw: line.to_owned(),
            });
        } else if line.contains("am_anr") {
            if let Some(fields) = bracket_fields(line) {
                if fields.len() >= 3 {
                    let summary = if fields.len() > 4 {
                        format!("ANR: {}", fields[4..].join(", "))
                    } else {
                        "ANR".into()
                    };
                    crashes.push(Crash {
                        kind: "anr".into(),
                        timestamp: timestamp(line),
                        process: fields[2].clone(),
                        summary,
                        raw: line.into(),
                    });
                }
            }
        }
        i += 1;
        if crashes.len() >= MAX_CRASHES {
            break;
        }
    }
    crashes.reverse();
    crashes
}

fn timestamp(line: &str) -> String {
    let value = line.get(..18).unwrap_or("");
    if value.len() == 18
        && value.as_bytes().get(2) == Some(&b'-')
        && value.as_bytes().get(5) == Some(&b' ')
    {
        value.into()
    } else {
        String::new()
    }
}

fn bracket_fields(line: &str) -> Option<Vec<String>> {
    let start = line.find('[')? + 1;
    let end = line[start..].find(']')? + start;
    Some(
        line[start..end]
            .split(',')
            .map(|v| v.trim().to_owned())
            .collect(),
    )
}

pub fn filtered<'a>(crashes: &'a [Crash], package: &str) -> Vec<&'a Crash> {
    crashes
        .iter()
        .filter(|c| {
            package.is_empty()
                || c.process == package
                || c.process
                    .strip_prefix(package)
                    .is_some_and(|suffix| suffix.starts_with(':'))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    const SAMPLE: &str = "06-24 02:45:49.127 1 1 E AndroidRuntime: FATAL EXCEPTION: main\n06-24 02:45:49.127 1 1 E AndroidRuntime: Process: com.example, PID: 1\n06-24 02:45:49.127 1 1 E AndroidRuntime: java.lang.RuntimeException: boom\n06-24 03:00:00.000 1 1 E ActivityManager: ANR in com.other.app (com.other/.Main)\n06-24 04:00:00.000 1 1 I am_anr : [0,2,com.latest,0,Input timed out]";
    #[test]
    fn parses_java_and_both_anr_forms_most_recent_first() {
        let c = parse(SAMPLE);
        assert_eq!(c.len(), 3);
        assert_eq!(c[0].process, "com.latest");
        assert_eq!(c[1].kind, "anr");
        assert!(c[2].summary.contains("RuntimeException"));
    }
    #[test]
    fn filters_by_exact_process_name() {
        let c = parse(SAMPLE);
        assert_eq!(filtered(&c, "com.example").len(), 1);
        assert!(filtered(&c, "missing").is_empty());
    }
    #[test]
    fn empty_and_noise_are_empty() {
        assert!(parse("").is_empty());
        assert!(parse("nothing").is_empty());
    }
    #[test]
    fn package_filter_is_exact_but_accepts_android_subprocesses() {
        let crashes = vec![
            Crash {
                kind: "java".into(),
                timestamp: String::new(),
                process: "com.foo".into(),
                summary: String::new(),
                raw: String::new(),
            },
            Crash {
                kind: "java".into(),
                timestamp: String::new(),
                process: "com.foo:worker".into(),
                summary: String::new(),
                raw: String::new(),
            },
            Crash {
                kind: "java".into(),
                timestamp: String::new(),
                process: "com.foobar".into(),
                summary: String::new(),
                raw: String::new(),
            },
        ];
        assert_eq!(filtered(&crashes, "com.foo").len(), 2);
    }
}
