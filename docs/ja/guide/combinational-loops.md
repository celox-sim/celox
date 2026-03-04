# 組み合わせループ

Celox は `always_comb` ブロックの依存関係を静的に解析してトポロジカル順序でスケジューリングします。依存グラフにサイクルを検出するとコンパイルが `CombinationalLoop` エラーで失敗します。

## 見かけ上のループ（`falseLoops`）

静的な依存グラフ上にサイクルが現れるものの、実行時には決してループしない場合です。最もよくある原因は、2 つのブランチが互いに逆側のパスに依存しているマルチプレクサです：

```veryl
module Top (
    sel: input  logic,
    i:   input  logic<2>,
    o:   output logic<2>,
) {
    var v: logic<2>;
    always_comb {
        if sel {
            v[0] = v[1];  // v[1] を読む
            v[1] = i[1];
        } else {
            v[0] = i[0];
            v[1] = v[0];  // v[0] を読む
        }
    }
    assign o = v;
}
```

`v[0]` と `v[1]` は互いに依存しているように見えますが、`v[0]→v[1]` は `sel=1` のとき、`v[1]→v[0]` は `sel=0` のときだけ起き、同時にループすることはありません。

そのままではコンパイルに失敗します。`falseLoops` でこのサイクルが安全であることを宣言します：

```typescript
const sim = Simulator.fromSource(SOURCE, "Top", {
  falseLoops: [
    { from: "v", to: "v" },
  ],
});
```

`from` と `to` にはサイクルに関係するシグナル名を指定します。Celox は SCC ブロックをサイクルの構造的な深さから算出した回数だけ実行して、実行順序によらずすべての値が正しく伝搬するようにします。

## シグナルパスの書き方

`from` と `to` にはシグナルパス文字列を指定します：

| パターン | 意味 |
|---------|------|
| `"v"` | トップレベルの変数 `v` |
| `"u_sub:i_data"` | 子インスタンス `u_sub` のポート `i_data` |
| `"u_a.u_b:x"` | `u_a` 内の `u_b` インスタンスのポート `x` |

## 関連資料

- [組み合わせ回路解析](/internals/combinational-analysis) -- 依存グラフの構築とスケジューリングの詳細。
- [テストの書き方](./writing-tests.md) -- シミュレータオプションの概要。
