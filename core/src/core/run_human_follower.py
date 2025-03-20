#!/usr/bin/env python3
"""
人間追跡ロボットを実行するスクリプト
"""
import argparse
import time
from core import HumanFollower


def main():
    parser = argparse.ArgumentParser(description="YOLOを用いた人間追跡ロボット")
    parser.add_argument(
        "--camera", type=int, default=0, help="カメラのインデックス (デフォルト: 0)"
    )
    parser.add_argument(
        "--confidence",
        type=float,
        default=0.5,
        help="検出信頼度の閾値 (デフォルト: 0.5)",
    )
    args = parser.parse_args()

    # 人間追跡機能の初期化
    follower = HumanFollower(
        camera_index=args.camera, confidence_threshold=args.confidence
    )

    try:
        # 追跡開始
        if follower.start():
            print("人間追跡を開始しました")
            print("終了するには Ctrl+C を押してください")

            # メインループ
            while True:
                time.sleep(1)

    except KeyboardInterrupt:
        print("\n終了します...")
    finally:
        follower.stop()
        print("終了しました")


if __name__ == "__main__":
    main()
