# S6b 外部阅读器样本

来源：用户在 `scripts/replay/rotated_2p.pdf`（两页，第 2 页 `/Rotate 90`）上用各阅读器画了
高亮/下划线/删除线/墨迹（各自支持的类型），原样保存。文件名里的阅读器名对应来源。

| 文件 | 来源 | 注释 |
| --- | --- | --- |
| `okular.pdf` | Okular | 2 高亮 + 1 下划线 + 1 删除线 + 1 墨迹（56 点，单 path），全部有 `/Contents` |
| `okular_without_content.pdf` | Okular | 同上，但用 Okular 删除了 `/Contents` |
| `evince.pdf` | Evince | 2 高亮 + 1 下划线 + 1 删除线（无墨迹），颜色都是默认黄 |
| `firefox.pdf` | Firefox | 2 高亮 + 1 墨迹（128 点，单 path）；Firefox 不支持下划线/删除线 |
| `adobe.pdf` | Adobe Acrobat | 5 条（2 高亮 + 下划线 + 删除线 + 墨迹 181 点）；**注释在对象流里**（ObjStm） |
| `foxit.pdf` | Foxit | 5 条（2 高亮 + 下划线 + 删除线 + 墨迹 104 点） |

用途：`tests/s6b_samples.rs`（读回字段）与 `app/tests/replay_scripts.rs`（只读层条数）。
这些文件只用于测试，不再被生产代码引用。
