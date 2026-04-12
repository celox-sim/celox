# ADR-0004: Software Modules Are Sequential-First

- **Date**: 2026-04-12
- **Status**: proposed

## Context

Celox で HDL モジュールの代わりに software 実装を差し込めるようにしたい。主な動機は、Vivado の MIG のような大きくて扱いづらい IP を、検証時だけ software 実装に置き換えたいことにある。

要件としては次が強い。

- HDL source 上では普通の module として存在してほしい
- どの階層でも普通の module と置き換えて使えてほしい
- host 側が module 名で software 実装へ差し替えたい

一方で、software module の内部 signal と combinational scheduling を真面目に扱おうとすると、次の問題がある。

- same-cycle の input -> output combinational path があると、global scheduler の dependency graph に参加する必要がある
- callback ベースで雑に `eval_comb()` すると性能の根幹が死ぬ
- dependency を手で書かせると correctness が壊れやすい
- host language で graph/IR を組ませる方向に寄せると、「それなら RTL を書けばよい」に近づく

特に MIG 代替のようなユースケースでは、本当に欲しいのは arbitrary combinational component 一般ではなく、

- 大きな内部 state を持つ
- bus-facing である
- cycle 単位で進む
- 入出力のどちらか、または両方が register で切られている

ような externally synchronous component であることが多い。

## Decision

software module は 2 種類に分けて扱う。

1. **Sequential software module**
2. **Combinational software module**

v1 では **Sequential software module のみを正式対象** とする。Combinational software module は将来課題として分離し、同じ仕組みで一気に解こうとしない。

### 1. Instantiation model

HDL source 上では普通の module を使い、host 側が module 名で software 実装へ差し替える。

- Veryl 側に専用構文は追加しない
- software module は top 専用ではなく、任意階層の instance と置き換え可能であるべき
- 親モジュールからは普通の instance と同じ port shape を持つ

### 2. v1 で許可する software module

v1 の software module は、**externally synchronous** でなければならない。

具体的には、module の外から観測可能な

- input -> output

の same-cycle combinational pathを持ってはならない。

言い換えると、

- 入力変化が同一 cycle 中にそのまま出力へ反映される component
- global comb scheduler の fixpoint に参加しないと正しく動かない component

は v1 の対象外とする。

この制約により、MIG 代替や bus-facing memory model のような stateful component を先に扱える。

### 3. Internal signal

Sequential software module は内部 state / internal signal を持ってよい。

ただし v1 では、それらは **module 内部に閉じた state** として扱い、global combinational scheduler に参加させない。

- 内部 state 更新は event / cycle 境界で行う
- 出力の更新も cycle 単位の contract に従う
- software module 内部の combinational detail を global dependency graph へ露出しない

### 4. Host API registration first

v1 は host 側 API で software module を登録する。

```rust
let sim = Simulator::builder(code, "Top")
    .register_software_module("MigModel", |ctx| Box::new(MigModel::new(ctx)))
    .build()?;
```

builder は module 名を key に registry を持ち、elaboration 時に一致した module instance を software 実装へ差し替える。

- HDL source 上では普通の module 定義を置く
- host は `module_name -> factory` を登録する
- factory は parameter / port metadata / instance path を受けて instance state を生成する

想定する最小 API は次の 2 層である。

```rust
pub trait SoftwareModuleFactory: Send + Sync + 'static {
    fn module_name(&self) -> &str;
    fn instantiate(&self, ctx: &SoftwareModuleContext) -> Box<dyn SequentialSoftwareModule>;
}

pub trait SequentialSoftwareModule: Send + 'static {
    fn on_event(&mut self, event: EventId, io: &mut SoftwareIo);
    fn settle_outputs(&mut self, io: &mut SoftwareIo);
}
```

`settle_outputs()` は pure combinational callback 一般ではなく、「外部から見て cycle 境界に閉じた output 導出」であることを要求する。

### 5. Execution model

software module instance は instance graph の正式メンバーとして扱う。

- 親から見える port shape は元の HDL module と一致しなければならない
- flatten / path 解決 / hierarchy 上は普通の instance として見える
- 中身の実行だけが software 実装になる

v1 の scheduler contract は次の通り。

1. 通常の HDL comb を評価
2. event 境界で software module の `on_event()` を実行
3. event 後に software module の `settle_outputs()` を実行
4. software module output の変化を downstream に反映

この contract は、same-cycle input -> output path を持たない externally synchronous component を対象にしている。

### 6. DLL/SO loading later

将来は host registry と同じ registry interface を DLL/SO から埋められるようにする。

```rust
pub trait SoftwareModuleRegistry {
    fn register(&mut self, factory: Box<dyn SoftwareModuleFactory>);
}

pub type RegisterFn = unsafe extern "C" fn(registry: &mut dyn SoftwareModuleRegistry);
```

- v1 は同一バイナリ内の host API 登録だけをサポートする
- v2 以降で `libloading` により `celox_register` をロードする
- 初期段階では Rust ABI 制約を許容する
- 必要なら将来 `abi_stable` 等で C ABI に寄せる

### 7. What is deferred

次は v1 では扱わない。

- same-cycle input -> output path を持つ software module
- global combinational scheduler への software node 参加
- software module 内部 signal の global dirty propagation
- arbitrary callback ベースの `eval_comb()` による汎用 combinational component
- DLL/SO の安定 ABI

これらは別 ADR / 別設計課題として扱う。

## Consequences

### Positive

- MIG 代替のような大きな stateful component を、RTL を無理に書かずに差し替えられる
- software module を module 名置換で使えるため、HDL 側の記述は自然
- いきなり global comb scheduler を壊さずに導入できる
- 「汎用 software module」と「comb scheduler 統合」という別難題を分離できる
- v1 は host API 登録だけで始められるため、ABI/配布問題を後回しにできる

### Negative

- v1 では truly combinational な software component は扱えない
- input -> output same-cycle path を持つ memory/peripheral model は対象外になる
- 将来 combinational software module をやる場合、別の execution model が必要
- DLL/SO ロードは別フェーズになる

### Open Questions

1. `settle_outputs()` をどこまで pure / side-effect-free に強制するか
2. `ready/valid` のような bus interface を v1 制約の下でどこまで自然に扱えるか
3. software module の internal state を introspection/debug でどこまで見せるか
4. module 名差し替え時に、元 HDL module body をどの段階で無視するか
5. parameterized module を factory にどう渡すか
