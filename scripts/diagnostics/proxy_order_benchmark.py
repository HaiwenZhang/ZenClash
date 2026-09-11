"""Measure the current node-order implementation in an optimized isolated harness.

Run after `cargo build -p zenclash-core --locked`. Uses generated data only.
The measured UI code uses rustc opt-level=3; linked core dependencies retain
their existing build profile. This is not a whole-app release benchmark.
"""
from pathlib import Path
import subprocess
import tempfile

root = Path(__file__).resolve().parents[2]
artifacts = list((root / "target/debug/deps").glob("libzenclash_core-*.rlib"))
if not artifacts:
    raise SystemExit("Build zenclash-core before running this benchmark.")
core = max(artifacts, key=lambda path: path.stat().st_mtime)
implementation = (root / "crates/zenclash-ui/src/pages/proxies/presentation.rs").read_text()
source = r'''
use zenclash_core::{ProxyGroup, ProxyNode, DelayHistory};
#[derive(Clone, Copy)] enum DelayTestFailure { Timeout, Failed }
fn test_key(group: &str, proxy: &str) -> String { format!("{group}\0{proxy}") }
mod measured {
''' + implementation + r'''
}
fn main() {
    use std::{collections::HashMap, hint::black_box, time::Instant};
    for count in [500, 5000, 20000] {
        let group = ProxyGroup {
            name: "test".into(),
            all: (0..count).map(|index| ProxyNode {
                name: format!("node-{index}"),
                history: vec![DelayHistory { delay: ((index * 7919) % 999 + 1) as u32, ..Default::default() }],
                ..Default::default()
            }).collect(),
            ..Default::default()
        };
        let failures = HashMap::from([
            (test_key("test", "node-0"), DelayTestFailure::Timeout),
            (test_key("test", "node-1"), DelayTestFailure::Failed),
        ]);
        let started = Instant::now();
        for _ in 0..500 {
            black_box(measured::visible_node_indices(black_box(&group), true, true, &failures));
        }
        let uncached = started.elapsed().as_secs_f64()*1e6/500.0;
        let orders = measured::GroupOrders::default();
        let expected = measured::visible_node_indices(&group, true, true, &failures);
        assert_eq!(&*orders.order(&group, true, true, &failures), expected);
        let started = Instant::now();
        for _ in 0..100000 {
            black_box(orders.order(black_box(&group), true, true, &failures));
        }
        let cached = started.elapsed().as_secs_f64()*1e6/100000.0;
        orders.invalidate("test");
        assert_eq!(&*orders.order(&group, true, true, &failures), expected);
        orders.clear();
        println!("nodes={count} uncached_mean_us={uncached:.2} cached_mean_us={cached:.4}");
    }
}
'''
with tempfile.TemporaryDirectory(prefix="zenclash-proxy-order-") as directory:
    directory = Path(directory)
    harness = directory / "proxy_order_benchmark.rs"
    executable = directory / "proxy_order_benchmark"
    harness.write_text(source)
    command = ["rustc", "--edition=2024", "-C", "opt-level=3", str(harness),
               "--extern", f"zenclash_core={core}", "-L", f"dependency={root}/target/debug/deps",
               "-o", str(executable)]
    for output in (root / "target/debug/build").glob("*/out"):
        command.extend(["-L", f"native={output}"])
    subprocess.run(command, check=True)
    subprocess.run([str(executable)], check=True)
