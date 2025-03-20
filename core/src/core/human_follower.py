import cv2
import numpy as np
import time
from ultralytics import YOLO
import math
import threading
from core.motor_controller import MotorController


class HumanFollower:
    def __init__(self, camera_index=0, confidence_threshold=0.5):
        self.camera_index = camera_index
        self.confidence_threshold = confidence_threshold
        self.model = YOLO("yolov8n.pt")  # 軽量なYOLOv8モデルを使用
        self.cap = None
        self.running = False
        self.detected_person = None
        self.frame_width = 640
        self.frame_height = 480
        self.center_x = self.frame_width // 2

        # モーター制御初期化
        self.motor = MotorController()
        self.motor.setup()
        self.direction = "stop"  # 現在の方向

        # スレッド処理用
        self.detection_thread = None

    def start(self):
        """人間追跡処理を開始"""
        if self.running:
            return

        self.cap = cv2.VideoCapture(self.camera_index)
        self.cap.set(cv2.CAP_PROP_FRAME_WIDTH, self.frame_width)
        self.cap.set(cv2.CAP_PROP_FRAME_HEIGHT, self.frame_height)

        if not self.cap.isOpened():
            print("カメラを開けませんでした")
            return False

        self.running = True
        self.detection_thread = threading.Thread(target=self._detection_loop)
        self.detection_thread.daemon = True
        self.detection_thread.start()
        return True

    def stop(self):
        """人間追跡処理を停止"""
        self.running = False
        if self.detection_thread:
            self.detection_thread.join(timeout=1.0)
        if self.cap:
            self.cap.release()
        self.motor.stop()
        self.motor.cleanup()

    def _detection_loop(self):
        """人間検出と追跡のメインループ"""
        while self.running:
            ret, frame = self.cap.read()
            if not ret:
                print("フレームを取得できませんでした")
                break

            # YOLOで人間検出
            results = self.model(frame, classes=0)  # クラス0は'person'

            # 人間が検出されたか確認
            self.detected_person = None
            best_confidence = self.confidence_threshold

            for result in results:
                boxes = result.boxes
                for box in boxes:
                    if box.cls[0] == 0:  # personクラス
                        confidence = float(box.conf[0])
                        if confidence > best_confidence:
                            best_confidence = confidence
                            x1, y1, x2, y2 = map(int, box.xyxy[0])
                            self.detected_person = {
                                "box": (x1, y1, x2, y2),
                                "confidence": confidence,
                                "center_x": (x1 + x2) // 2,
                                "center_y": (y1 + y2) // 2,
                                "width": x2 - x1,
                                "height": y2 - y1,
                            }

            # 人間が検出された場合は追跡処理
            if self.detected_person:
                self._follow_person()
            else:
                self._motor_control("stop")

            time.sleep(0.05)  # 少し待機して CPU 使用率を下げる

    def _follow_person(self):
        """検出された人間を追跡"""
        person = self.detected_person

        # 人間の位置に基づいて移動方向を決定
        person_center_x = person["center_x"]
        person_width = person["width"]

        # 検出された人間のサイズから距離を推定
        # ここでは簡易的に、幅が大きいほど近いと判断
        distance_factor = self.frame_width / max(person_width, 1)

        # 方向判定のためのマージン
        margin = self.frame_width * 0.1

        if distance_factor < 2.0:  # 非常に近い
            self._motor_control("backward")
        elif distance_factor > 6.0:  # 遠い
            if abs(person_center_x - self.center_x) < margin:
                self._motor_control("forward")
            elif person_center_x < self.center_x:
                self._motor_control("left_forward")
            else:
                self._motor_control("right_forward")
        else:  # 適切な距離
            if abs(person_center_x - self.center_x) < margin:
                self._motor_control("stop")
            elif person_center_x < self.center_x:
                self._motor_control("left")
            else:
                self._motor_control("right")

    def _motor_control(self, direction):
        """モーターを制御して指定された方向に動く"""
        if direction == self.direction:
            return  # 同じ方向なら何もしない

        self.direction = direction

        # モーター制御
        if direction == "forward":
            self.motor.forward()
        elif direction == "backward":
            self.motor.backward()
        elif direction == "left":
            self.motor.left()
        elif direction == "right":
            self.motor.right()
        elif direction == "left_forward":
            self.motor.left_forward()
        elif direction == "right_forward":
            self.motor.right_forward()
        elif direction == "stop":
            self.motor.stop()


def main():
    follower = HumanFollower()
    try:
        if follower.start():
            print("人間追跡を開始しました。終了するには Ctrl+C を押してください。")
            while True:
                time.sleep(1)
    except KeyboardInterrupt:
        print("終了します...")
    finally:
        follower.stop()


if __name__ == "__main__":
    main()
