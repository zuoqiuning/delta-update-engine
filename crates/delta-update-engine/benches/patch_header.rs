use criterion::{criterion_group, criterion_main, Criterion};

use delta_update_engine::patch::container::PatchHeader;

fn parse_header(c: &mut Criterion) {
    let header = PatchHeader {
        format: 1,
        strategy_id: 1,
        codec: delta_update_engine::patch::container::Codec::Zstd,
        target_size: 10 * 1024 * 1024,
        source_sha256: [1; 32],
        target_sha256: [2; 32],
    };
    let encoded = header.encode();
    let mut buffer = encoded.to_vec();
    buffer.extend_from_slice(&[0_u8; 1024]);

    let mut group = c.benchmark_group("patch_header");
    group.bench_function("parse", |b| {
        b.iter(|| PatchHeader::parse(&buffer).expect("valid header"));
    });
    group.finish();
}

criterion_group!(benches, parse_header);
criterion_main!(benches);
