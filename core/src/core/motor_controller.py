import time


class MotorController:
    def __init__(self):
        # モーター制御のためのピン番号などの設定
        # 実際のハードウェアに合わせて設定する必要があります
        self.motor_pins = {
            "left_forward": None,
            "left_backward": None,
            "right_forward": None,
            "right_backward": None,
        }
        self.speed_left = 50
        self.speed_right = 50

    def setup(self):
        """モーター制御のためのセットアップを行う"""
        # ここでGPIO初期化などを行う
        # 実際のハードウェアによって異なります
        pass

    def forward(self, speed=None):
        """前進する"""
        if speed is not None:
            self.speed_left = speed
            self.speed_right = speed

        # 前進のための制御信号を送信
        # 実際のハードウェアによって異なります
        print(
            f"前進: 左モーター速度={self.speed_left}, 右モーター速度={self.speed_right}"
        )

    def backward(self, speed=None):
        """後退する"""
        if speed is not None:
            self.speed_left = speed
            self.speed_right = speed

        # 後退のための制御信号を送信
        # 実際のハードウェアによって異なります
        print(
            f"後退: 左モーター速度={self.speed_left}, 右モーター速度={self.speed_right}"
        )

    def left(self, speed=None):
        """左に旋回する"""
        if speed is not None:
            self.speed_left = speed
            self.speed_right = speed

        # 左旋回のための制御信号を送信
        # 実際のハードウェアによって異なります
        print(
            f"左旋回: 左モーター速度={self.speed_left}, 右モーター速度={self.speed_right}"
        )

    def right(self, speed=None):
        """右に旋回する"""
        if speed is not None:
            self.speed_left = speed
            self.speed_right = speed

        # 右旋回のための制御信号を送信
        # 実際のハードウェアによって異なります
        print(
            f"右旋回: 左モーター速度={self.speed_left}, 右モーター速度={self.speed_right}"
        )

    def left_forward(self, speed=None):
        """左前方向に進む"""
        if speed is not None:
            self.speed_left = int(speed * 0.5)
            self.speed_right = speed

        # 左前進のための制御信号を送信
        # 実際のハードウェアによって異なります
        print(
            f"左前進: 左モーター速度={self.speed_left}, 右モーター速度={self.speed_right}"
        )

    def right_forward(self, speed=None):
        """右前方向に進む"""
        if speed is not None:
            self.speed_left = speed
            self.speed_right = int(speed * 0.5)

        # 右前進のための制御信号を送信
        # 実際のハードウェアによって異なります
        print(
            f"右前進: 左モーター速度={self.speed_left}, 右モーター速度={self.speed_right}"
        )

    def stop(self):
        """停止する"""
        # 停止のための制御信号を送信
        # 実際のハードウェアによって異なります
        print("停止")

    def cleanup(self):
        """終了時のクリーンアップ処理"""
        # GPIO等のクリーンアップ処理
        # 実際のハードウェアによって異なります
        self.stop()
        print("モーターコントローラーをクリーンアップしました")


# テスト用
if __name__ == "__main__":
    motor = MotorController()
    motor.setup()

    try:
        print("前進テスト")
        motor.forward(speed=50)
        time.sleep(2)

        print("左旋回テスト")
        motor.left(speed=50)
        time.sleep(2)

        print("右旋回テスト")
        motor.right(speed=50)
        time.sleep(2)

        print("後退テスト")
        motor.backward(speed=50)
        time.sleep(2)

        print("停止テスト")
        motor.stop()
    finally:
        motor.cleanup()
