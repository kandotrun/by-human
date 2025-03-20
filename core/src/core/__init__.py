def hello() -> str:
    return "Hello from core!"


from core.human_follower import HumanFollower
from core.motor_controller import MotorController

__all__ = ["hello", "HumanFollower", "MotorController"]
