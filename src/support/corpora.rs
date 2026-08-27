// Shared across targets the same way `upstream_zstd.rs` is; see the note there.
// `CorpusCase::description` in particular is read only by the report writer.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DictKind {
    None,
    Raw,
    Trained,
}

pub struct CorpusCase {
    pub name: &'static str,
    pub description: &'static str,
    pub input: Vec<u8>,
    pub dict_kind: DictKind,
}

pub fn build_small_alphabet_pattern(size: usize) -> Vec<u8> {
    (0..size)
        .map(|index| match index & 0x0f {
            0..=8 => b'A',
            9..=12 => b'B',
            13..=14 => b'C',
            _ => b'D',
        })
        .collect()
}

pub fn build_repeated_chunk_pattern(size: usize) -> Vec<u8> {
    const CHUNK: &[u8] = b"zstd-rs-window-repcode-pattern-0123456789ABCDEF";

    let mut out = Vec::with_capacity(size);
    while out.len() < size {
        let remaining = size - out.len();
        let take = remaining.min(CHUNK.len());
        out.extend_from_slice(&CHUNK[..take]);
    }
    out
}

pub fn build_raw_dictionary_input(size: usize) -> Vec<u8> {
    let statuses = ["active", "pending", "disabled"];
    let roles = ["admin", "analyst", "operator"];
    let regions = ["us-central", "us-east", "eu-west"];

    let mut out = Vec::with_capacity(size);
    let mut user_id = 1_000u32;
    while out.len() < size {
        let status = statuses[user_id as usize % statuses.len()];
        let role = roles[(user_id as usize / 2) % roles.len()];
        let region = regions[(user_id as usize / 3) % regions.len()];
        let record = format!(
            "GET /api/v1/users?id={user_id}&status={status} HTTP/1.1\r\n\
Host: example.internal\r\n\
Accept: application/json\r\n\
{{\"status\":\"{status}\",\"role\":\"{role}\",\"region\":\"{region}\"}}\n"
        );
        let remaining = size - out.len();
        out.extend_from_slice(&record.as_bytes()[..remaining.min(record.len())]);
        user_id += 1;
    }
    out
}

pub fn build_trained_dictionary_input(size: usize) -> Vec<u8> {
    let order_statuses = ["open", "closed", "pending"];
    let invoice_statuses = ["draft", "final", "paid"];
    let build_states = ["running", "passed", "failed"];
    let branches = ["main", "release", "hotfix"];
    let regions = ["us-east", "eu-west", "ap-south"];

    let mut out = Vec::with_capacity(size);
    let mut index = 0u32;
    while out.len() < size {
        let customer_id = 10_000 + index * 7;
        let project_id = 4_000 + index * 3;
        let build_id = 9_000 + index * 5;
        let status = order_statuses[index as usize % order_statuses.len()];
        let invoice_status = invoice_statuses[index as usize % invoice_statuses.len()];
        let build_state = build_states[index as usize % build_states.len()];
        let branch = branches[index as usize % branches.len()];
        let region = regions[index as usize % regions.len()];
        let record = match index % 3 {
            0 => format!(
                "GET /v2/customers/{customer_id}/orders?status={status}&limit=50\n\
{{\"customer_id\":{customer_id},\"status\":\"{status}\",\"region\":\"{region}\",\"items\":[{{\"sku\":\"A-{sku}\",\"qty\":{qty}}}]}}\n",
                sku = 100 + (index % 17),
                qty = 1 + (index % 4),
            ),
            1 => format!(
                "POST /v2/customers/{customer_id}/invoices\n\
{{\"customer_id\":{customer_id},\"currency\":\"USD\",\"total\":{total},\"status\":\"{invoice_status}\",\"region\":\"{region}\"}}\n",
                total = 1_500 + index * 11,
            ),
            _ => format!(
                "PATCH /v2/projects/{project_id}/builds/{build_id}\n\
{{\"project\":{project_id},\"build\":{build_id},\"state\":\"{build_state}\",\"branch\":\"{branch}\",\"artifact\":\"bundle.tar\"}}\n",
            ),
        };

        let remaining = size - out.len();
        out.extend_from_slice(&record.as_bytes()[..remaining.min(record.len())]);
        index += 1;
    }
    out
}

pub fn build_json_records_pattern(size: usize) -> Vec<u8> {
    let services = ["api", "billing", "search", "worker"];
    let regions = ["us-east-1", "us-west-2", "eu-west-1"];
    let statuses = ["ok", "degraded", "failed"];

    let mut out = Vec::with_capacity(size);
    let mut index = 0u32;
    while out.len() < size {
        let service = services[index as usize % services.len()];
        let region = regions[(index as usize / 2) % regions.len()];
        let status = statuses[(index as usize / 5) % statuses.len()];
        let record = format!(
            "{{\"ts\":\"2026-03-08T12:{minute:02}:{second:02}Z\",\"service\":\"{service}\",\"region\":\"{region}\",\"status\":\"{status}\",\"latency_ms\":{latency},\"req_id\":\"req-{req_id:08x}\",\"user\":{user_id}}}\n",
            minute = index % 60,
            second = (index * 7) % 60,
            latency = 20 + (index * 13) % 700,
            req_id = index.wrapping_mul(2_654_435_761),
            user_id = 10_000 + index * 3,
        );
        let remaining = size - out.len();
        out.extend_from_slice(&record.as_bytes()[..remaining.min(record.len())]);
        index += 1;
    }
    out
}

pub fn build_log_lines_pattern(size: usize) -> Vec<u8> {
    let levels = ["INFO", "WARN", "ERROR"];
    let components = ["gateway", "scheduler", "replicator", "api"];
    let actions = ["read", "write", "flush", "rebalance"];

    let mut out = Vec::with_capacity(size);
    let mut index = 0u32;
    while out.len() < size {
        let level = levels[index as usize % levels.len()];
        let component = components[(index as usize / 2) % components.len()];
        let action = actions[(index as usize / 3) % actions.len()];
        let record = format!(
            "2026-03-08T12:{minute:02}:{second:02}Z {level} {component} node={node:02} shard={shard:03} action={action} duration_ms={duration} status={status} trace={trace:016x}\n",
            minute = index % 60,
            second = (index * 11) % 60,
            node = index % 24,
            shard = (index * 17) % 512,
            duration = 1 + (index * 29) % 3_000,
            status = if index.is_multiple_of(11) {
                "retry"
            } else {
                "ok"
            },
            trace = (index as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15),
        );
        let remaining = size - out.len();
        out.extend_from_slice(&record.as_bytes()[..remaining.min(record.len())]);
        index += 1;
    }
    out
}

pub fn build_mixed_entropy_pattern(size: usize) -> Vec<u8> {
    let compressible = build_repeated_chunk_pattern(8 * 1024);
    let mut out = Vec::with_capacity(size);
    let mut rng = XorShift64::new(0x1234_5678_9ABC_DEF0);

    while out.len() < size {
        let use_random = (out.len() / 8_192) % 3 == 2;
        if use_random {
            let remaining = size - out.len();
            let chunk_len = remaining.min(8 * 1024);
            for _ in 0..chunk_len {
                out.push((rng.next_u64() & 0xff) as u8);
            }
        } else {
            let remaining = size - out.len();
            let take = remaining.min(compressible.len());
            out.extend_from_slice(&compressible[..take]);
        }
    }

    out
}

pub fn build_wikipedia_pattern(size: usize) -> Vec<u8> {
    // Deterministic public-domain-style encyclopaedic text with repeated
    // structural patterns and moderate vocabulary churn.
    let topics = [
        "mathematics",
        "physics",
        "biology",
        "chemistry",
        "history",
        "geography",
        "astronomy",
        "philosophy",
        "literature",
        "economics",
    ];
    let verbs = ["is", "was", "has been", "remains", "became"];
    let adjectives = [
        "significant",
        "notable",
        "fundamental",
        "important",
        "early",
    ];
    let sentences = [
        "The discipline emerged during the {adj} era and {verb} central to modern research.",
        "According to several sources, the concept {verb} widely studied since antiquity.",
        "Recent developments have shown that the topic {verb} relevant to applied science.",
        "Scholars note that {topic} {verb} a {adj} area of inquiry.",
        "In the context of {topic}, this {verb} a {adj} contribution to knowledge.",
    ];

    let mut out = Vec::with_capacity(size);
    let mut index = 0u32;
    while out.len() < size {
        let topic = topics[index as usize % topics.len()];
        let verb = verbs[(index as usize / 3) % verbs.len()];
        let adj = adjectives[(index as usize / 5) % adjectives.len()];
        let template = sentences[index as usize % sentences.len()];
        let section = if index.is_multiple_of(7) {
            format!("\n== {topic} ==\n\n")
        } else {
            String::new()
        };
        let sentence = template
            .replace("{topic}", topic)
            .replace("{verb}", verb)
            .replace("{adj}", adj);
        let ref_id = 100 + (index.wrapping_mul(37) % 900);
        let paragraph = format!(
            "{section}{sentence} See the article on {topic} for details (ref [{ref_id}]).\n",
        );
        let remaining = size - out.len();
        out.extend_from_slice(&paragraph.as_bytes()[..remaining.min(paragraph.len())]);
        index += 1;
    }
    out
}

pub fn build_tabular_csv_pattern(size: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(size);
    let header = "id,timestamp,region,category,value,status,metric_a,metric_b\n";
    out.extend_from_slice(header.as_bytes());

    let regions = ["us-east", "us-west", "eu-west", "eu-central", "ap-south"];
    let categories = ["compute", "storage", "network", "database", "cache"];
    let statuses = ["active", "idle", "degraded"];

    let mut index = 0u32;
    while out.len() < size {
        let region = regions[index as usize % regions.len()];
        let category = categories[(index as usize / 2) % categories.len()];
        let status = statuses[(index as usize / 7) % statuses.len()];
        let row = format!(
            "{id},2026-03-08T{hour:02}:{minute:02}:{second:02}Z,{region},{category},{value:.2},{status},{ma},{mb}\n",
            id = 10000 + index,
            hour = (index / 60) % 24,
            minute = index % 60,
            second = (index * 7) % 60,
            value = 0.5 + (index as f64 * 0.013) % 100.0,
            ma = 100 + (index * 17) % 5000,
            mb = 200 + (index * 31) % 3000,
        );
        let remaining = size - out.len();
        out.extend_from_slice(&row.as_bytes()[..remaining.min(row.len())]);
        index += 1;
    }
    out
}

pub fn build_binary_structured_pattern(size: usize) -> Vec<u8> {
    // Repeating binary records with fixed headers and variable payloads,
    // simulating protobuf/flatbuffers-like serialised data.
    let mut out = Vec::with_capacity(size);
    let mut index = 0u32;

    // Fixed magic header repeated at each record boundary.
    let magic: [u8; 4] = [0x5A, 0x53, 0x54, 0x44]; // "ZSTD"
    // Repeatable payload templates (simulates repeated message types).
    let templates: [&[u8]; 4] = [
        b"\x08\x01\x12\x10user_session_key\x18\x00\x20\x01\x2a\x08",
        b"\x08\x02\x12\x0emetric_payload\x18\x00\x20\x02\x2a\x04",
        b"\x08\x03\x12\x0blog_message\x18\x01\x20\x03\x2a\x10",
        b"\x08\x04\x12\x0fheartbeat_check\x18\x00\x20\x00\x2a\x06",
    ];

    while out.len() < size {
        let remaining = size - out.len();
        let record_type = (index % 8) as u16;
        let template = templates[index as usize % templates.len()];
        if remaining < 8 {
            out.extend_from_slice(&magic[..remaining]);
            break;
        }
        // 4-byte magic + 2-byte type + 2-byte length placeholder
        out.extend_from_slice(&magic);
        out.extend_from_slice(&record_type.to_le_bytes());
        let len_pos = out.len();
        out.extend_from_slice(&0u16.to_le_bytes());
        // Payload: template bytes + deterministic counter fields
        let payload_start = out.len();
        let template_take = remaining.saturating_sub(8).min(template.len());
        out.extend_from_slice(&template[..template_take]);
        // Deterministic varying fields (timestamps, counters).
        let field_a = index.wrapping_mul(0x9E37_79B9);
        let field_b = (index / 4).wrapping_add(0x1234_5678);
        let extra_remaining = remaining.saturating_sub(8 + template_take).min(8);
        let fields = [field_a.to_le_bytes(), field_b.to_le_bytes()].concat();
        out.extend_from_slice(&fields[..extra_remaining]);
        // Patch length.
        let payload_len = (out.len() - payload_start) as u16;
        out[len_pos..len_pos + 2].copy_from_slice(&payload_len.to_le_bytes());
        index += 1;
    }
    out
}

pub fn build_pseudorandom_pattern(size: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(size);
    let mut rng = XorShift64::new(0x0DDB_1A5E_5EED_1234);
    for _ in 0..size {
        out.push((rng.next_u64() >> 32) as u8);
    }
    out
}

pub fn benchmark_report_cases(size: usize) -> Vec<CorpusCase> {
    vec![
        CorpusCase {
            name: "small-alphabet",
            description: "Four-symbol high-redundancy synthetic text.",
            input: build_small_alphabet_pattern(size),
            dict_kind: DictKind::None,
        },
        CorpusCase {
            name: "repeated-chunk",
            description: "Single repeated chunk that stresses match finding and repcodes.",
            input: build_repeated_chunk_pattern(size),
            dict_kind: DictKind::None,
        },
        CorpusCase {
            name: "json-records",
            description: "Structured JSON-like service records with repeated keys and modest value churn.",
            input: build_json_records_pattern(size),
            dict_kind: DictKind::None,
        },
        CorpusCase {
            name: "log-lines",
            description: "Timestamped log-style lines with stable fields and changing numeric values.",
            input: build_log_lines_pattern(size),
            dict_kind: DictKind::None,
        },
        CorpusCase {
            name: "mixed-entropy",
            description: "Alternating compressible and incompressible 8 KiB regions.",
            input: build_mixed_entropy_pattern(size),
            dict_kind: DictKind::None,
        },
        CorpusCase {
            name: "wikipedia",
            description: "Encyclopaedic prose with structural repetition and moderate vocabulary.",
            input: build_wikipedia_pattern(size),
            dict_kind: DictKind::None,
        },
        CorpusCase {
            name: "tabular-csv",
            description: "CSV rows with column repetition and numeric variation.",
            input: build_tabular_csv_pattern(size),
            dict_kind: DictKind::None,
        },
        CorpusCase {
            name: "binary-structured",
            description: "Repeating binary records with fixed headers and variable payloads.",
            input: build_binary_structured_pattern(size),
            dict_kind: DictKind::None,
        },
        CorpusCase {
            name: "pseudorandom",
            description: "Deterministic incompressible-looking bytes.",
            input: build_pseudorandom_pattern(size),
            dict_kind: DictKind::None,
        },
        CorpusCase {
            name: "raw-dictionary",
            description: "HTTP-like records aligned with the raw-content dictionary fixture.",
            input: build_raw_dictionary_input(size),
            dict_kind: DictKind::Raw,
        },
        CorpusCase {
            name: "trained-dictionary",
            description: "Structured multi-endpoint records aligned with the trained dictionary fixture.",
            input: build_trained_dictionary_input(size),
            dict_kind: DictKind::Trained,
        },
    ]
}

struct XorShift64 {
    state: u64,
}

impl XorShift64 {
    fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    fn next_u64(&mut self) -> u64 {
        let mut value = self.state;
        value ^= value << 13;
        value ^= value >> 7;
        value ^= value << 17;
        self.state = value;
        value
    }
}
