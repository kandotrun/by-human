#!/usr/bin/env python
"""
CLIインターフェースのエントリーポイント
"""

import os
import sys
import argparse
import re
from typing import List, Optional

from rich.console import Console
from rich.panel import Panel
from rich.markdown import Markdown
from rich.syntax import Syntax
from rich.table import Table
from rich.prompt import Prompt, Confirm

from .main import PersonalAI

console = Console()


def main():
    """
    メイン関数
    """
    # コマンドライン引数のパース
    parser = argparse.ArgumentParser(description="パーソナライズAIアシスタント")
    parser.add_argument(
        "command",
        nargs="?",
        help="実行するコマンド (profile, history, remember, image, search, link など)",
    )
    parser.add_argument("args", nargs="*", help="コマンドに渡す引数")
    parser.add_argument("--user", help="ユーザーID", default="default_user")
    parser.add_argument("--model", help="使用するOllamaモデル", default="llama3.3")

    args = parser.parse_args()
    user_id = (
        f"cli_{args.user}"  # CLIモードのユーザーIDには'cli_'プレフィックスを付ける
    )

    # AIインスタンスの初期化
    ai = PersonalAI(user_id=user_id, ollama_model=args.model)

    # コマンドがある場合はコマンドを実行
    if args.command:
        command = args.command.lower()
        command_args = args.args

        # linkコマンドの特別な処理
        if command == "link":
            result = ai._handle_link(command_args)
            display_result(result)
            return

        if command == "profile":
            # profile サブコマンドの処理
            if not command_args:
                # 引数がない場合は show サブコマンドを実行
                result = ai._handle_profile(["show"])
            else:
                result = ai._handle_profile(command_args)
            display_result(result)

        elif command == "history":
            # history サブコマンドの処理
            if not command_args:
                # 引数がない場合は list サブコマンドを実行
                result = ai._handle_history(["list"])
            else:
                result = ai._handle_history(command_args)
            display_result(result)

        elif command == "remember":
            # remember コマンドの処理
            if not command_args:
                console.print("[bold red]エラー:[/] 記憶する内容を指定してください")
                return
            result = ai._handle_remember(command_args)
            display_result(result)

        elif command == "image":
            # image サブコマンドの処理
            if not command_args:
                # 引数がない場合は list サブコマンドを実行
                result = ai._handle_image(["list"])
            else:
                result = ai._handle_image(command_args)
            display_result(result)

        elif command == "search":
            # search コマンドの処理
            if not command_args:
                console.print("[bold red]エラー:[/] 検索クエリを指定してください")
                return
            result = ai._handle_search(command_args)
            display_result(result)

        elif command == "clear":
            # clear コマンドの処理
            result = ai._handle_clear([])
            display_result(result)

        elif command == "help":
            # help コマンドの処理
            result = ai._handle_help([])
            display_result(result)

        else:
            # その他のコマンドはAIに処理してもらう
            result = ai.process_command(f"/{command} {' '.join(command_args)}")
            if result:
                display_result(result)
            else:
                console.print(f"[bold red]エラー:[/] 不明なコマンド '{command}'")
        return

    # 対話モード
    console.print(
        Panel.fit(
            "パーソナライズAIアシスタント\n"
            "終了するには 'exit' または 'quit' と入力してください。\n"
            "コマンド一覧を表示するには 'help' と入力してください。",
            title="Welcome",
            border_style="blue",
        )
    )

    # 対話ループ
    while True:
        try:
            # ユーザー入力
            user_input = Prompt.ask("\n[bold blue]>>[/]")

            # 終了コマンド
            if user_input.lower() in ["exit", "quit"]:
                console.print("[bold green]さようなら！[/]")
                break

            # 空の入力はスキップ
            if not user_input.strip():
                continue

            # personal-ai linkコマンドを検出して処理
            if user_input.startswith("personal-ai link"):
                parts = user_input.split(maxsplit=2)
                if len(parts) == 3:
                    link_code = parts[2]
                    console.print("リンクコードを処理中...")
                    result = ai._handle_link([link_code])
                    display_result(result)
                else:
                    console.print("[bold red]エラー:[/] リンクコードを指定してください")
                continue

            # コマンドかどうかを確認
            if user_input.startswith("/") or user_input.startswith("!"):
                if user_input.startswith("!"):
                    user_input = "/" + user_input[1:]

                # コマンドを処理
                result = ai.process_command(user_input)
                if result:
                    display_result(result)
                continue

            # 通常の会話として処理
            response = ai.generate_response(user_input)

            # 応答を表示
            console.print(Markdown(response))

        except KeyboardInterrupt:
            console.print("\n[bold yellow]中断されました。[/]")
            break
        except Exception as e:
            console.print(f"[bold red]エラー:[/] {str(e)}")


def display_result(result: str) -> None:
    """
    結果を表示

    Args:
        result: 表示する結果
    """
    # 表示するコンテンツをフォーマット
    if "#" in result or "```" in result:
        # マークダウンとして表示
        console.print(Markdown(result))
    else:
        # 通常のテキストとして表示
        console.print(result)


if __name__ == "__main__":
    main()
