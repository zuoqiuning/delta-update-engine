use criterion::{criterion_group, criterion_main, Criterion, Throughput};

fn hash_bytes(c: &mut Criterion) {
    let data = vec![0x5a_u8; 1024 * 1024];
    let mut group = c.benchmark_group("hash");
    group.throughput(Throughput::Bytes(data.len() as u64));
    group.bench_function("sha256_1mib", |b| {
        b.iter(|| delta_update_engine::crypto::sha256_bytes(&data));
    });
    group.finish();
}

criterion_group!(benches, hash_bytes);
criterion_main!(benches);
