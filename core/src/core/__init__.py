"""
パーソナライズAIのコアモジュール
"""

from .main import PersonalAI

__version__ = "0.1.0"


# CLIエントリーポイントを追加
def main():
    """CLIエントリーポイント"""
    from .cli import main as cli_main

    cli_main()


def hello() -> str:
    return "Hello from core!"


from core.human_follower import HumanFollower
from core.motor_controller import MotorController

__all__ = ["hello", "HumanFollower", "MotorController"]
