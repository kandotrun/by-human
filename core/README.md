# Core

AI ロボット制御コア

## 人間追跡機能

YOLOv8を用いて人間を検出し、ロボットが追従する機能を提供します。

### 依存関係のインストール

```bash
pip install -e .
```

### 使用方法

```python
from core import HumanFollower

# 初期化
follower = HumanFollower(camera_index=0)  # カメラのインデックスを指定

try:
    # 追跡開始
    if follower.start():
        print("人間追跡開始")
        # ここにメインのロジックを書く
        # 例えば、別の制御ループやイベント処理など
except KeyboardInterrupt:
    print("終了")
finally:
    # 終了時のクリーンアップ
    follower.stop()
```

### モーター制御のカスタマイズ

実際のハードウェアに合わせて、`motor_controller.py`を編集して適切なモーター制御を実装してください。デフォルトでは、各種モーション（前進、後退、左右旋回など）のメソッドが提供されていますが、実際のハードウェア制御部分は実装する必要があります。
