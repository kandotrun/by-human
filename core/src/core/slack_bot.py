#!/usr/bin/env python
"""
Slackボットとして動作するパーソナライズAI - Ollama Llama 3.3版

このスクリプトは、main.pyで実装されたパーソナライズAIシステムをSlack上で利用できるようにします。
各ユーザーごとに独立したメモリを保持し、会話履歴やプロファイル情報を管理します。
"""

import os
import logging
import re
import json
import tempfile
import requests
import base64
import time
import csv
import pandas as pd
from typing import Dict, Any, Optional, List, Tuple
from datetime import datetime
from dotenv import load_dotenv
from slack_bolt import App
from slack_bolt.adapter.socket_mode import SocketModeHandler
from PIL import Image
import io
import ollama  # ollamaライブラリを追加
import asyncio
from functools import partial
import sqlite3
import hashlib
import threading
import atexit

# MCP関連のインポート
try:
    from mcp import ClientSession, StdioServerParameters
    from mcp.client.stdio import stdio_client
    from langchain_mcp_adapters.tools import load_mcp_tools
    from langchain_mcp_adapters.client import MultiServerMCPClient
    MCP_AVAILABLE = True
except ImportError:
    MCP_AVAILABLE = False
    logger.warning("MCP関連のライブラリがインストールされていないため、ターミナル制御機能が制限されます")

# browser-use関連のインポート（インストールされている場合のみ）
BROWSER_USE_AVAILABLE = False
try:
    from browser_use import Agent as BrowserAgent
    from langchain_ollama import ChatOllama
    from playwright.sync_api import sync_playwright
    from langchain.chains import LLMChain
    from langchain.prompts import PromptTemplate

    BROWSER_USE_AVAILABLE = True
except ImportError:
    pass

try:
    import PyPDF2

    PDF_SUPPORT = True
except ImportError:
    PDF_SUPPORT = False
    logger.warning("PyPDF2がインストールされていないため、PDF解析機能が制限されます")

# main.pyからPersonalAIクラスをインポート
from .main import PersonalAI

# .env ファイルから環境変数を読み込み
load_dotenv()

# ロガーの設定
logging.basicConfig(
    level=logging.DEBUG,  # より詳細なログを出力するためDEBUGレベルに変更
    format="%(asctime)s - %(name)s - %(levelname)s - %(message)s",
)
logger = logging.getLogger(__name__)

# Slack API設定
SLACK_BOT_TOKEN = os.environ.get("SLACK_BOT_TOKEN")
SLACK_APP_TOKEN = os.environ.get("SLACK_APP_TOKEN")
OLLAMA_MODEL = os.environ.get("OLLAMA_MODEL", "llama3.3")
IMAGE_MODEL = os.environ.get("IMAGE_MODEL", "gemma3:27b")
OLLAMA_HOST = os.environ.get("OLLAMA_HOST", "http://localhost:11434")
DEBUG_MODE = os.environ.get("DEBUG_MODE", "false").lower() in ["true", "1", "yes"]
DEBUG_IMAGES_DIR = os.environ.get("DEBUG_IMAGES_DIR", "debug_images")

# 代替画像モデルのリスト（優先順位順）
FALLBACK_IMAGE_MODELS = ["llava", "bakllava", "llava:latest"]

# Slackアプリの初期化
app = App(token=SLACK_BOT_TOKEN)

# WebClientを多くの場所で使うので、インポートを上部に移動
from slack_sdk.web import WebClient

# MCP関連のグローバル変数
_mcp_client = None
_mcp_tools = None

class MCPTerminalController:
    """
    MCPを使用してターミナルを制御するためのクラス
    """
    def __init__(self):
        self.client = None
        self.tools = None
        self.initialized = False
        self.server_process = None
    
    async def initialize(self):
        """MCPクライアントとツールを初期化する"""
        if not MCP_AVAILABLE:
            logger.warning("MCP関連のライブラリがインストールされていないため、ターミナル制御を初期化できません")
            return False
            
        try:
            logger.info("MCPターミナルコントローラを初期化中...")
            
            # ターミナルMCPサーバーのパラメータ
            terminal_params = StdioServerParameters(
                command="npx",
                args=["y", "@joshuarileydev/terminal-mcp-server"]
            )
            
            # マルチサーバークライアントの作成
            self.client = MultiServerMCPClient()
            
            # ターミナルサーバーを追加
            await self.client.add_server('terminal', stdio_client, terminal_params)
            
            # 利用可能なツールをロード
            self.tools = await load_mcp_tools(self.client)
            
            logger.info(f"MCPツールが正常にロードされました: {len(self.tools)}")
            for tool in self.tools:
                logger.debug(f"利用可能なMCPツール: {tool.name}")
            
            self.initialized = True
            return True
        except Exception as e:
            logger.error(f"MCP初期化エラー: {str(e)}", exc_info=True)
            return False
    
    async def execute_command(self, command, working_directory=None):
        """
        ターミナルコマンドを実行する
        
        Args:
            command: 実行するコマンド
            working_directory: 作業ディレクトリ（省略可）
            
        Returns:
            コマンド実行結果
        """
        if not self.initialized:
            success = await self.initialize()
            if not success:
                return "MCPの初期化に失敗しました。"
        
        try:
            # ターミナルツールを探す
            terminal_tool = None
            for tool in self.tools:
                if tool.name == "terminal":
                    terminal_tool = tool
                    break
            
            if not terminal_tool:
                return "ターミナルツールが見つかりません。"
            
            # 作業ディレクトリの設定（指定がある場合）
            if working_directory:
                cd_result = await terminal_tool.invoke({"command": f"cd {working_directory}"})
                logger.debug(f"作業ディレクトリ変更結果: {cd_result}")
            
            # コマンド実行
            result = await terminal_tool.invoke({"command": command})
            logger.info(f"コマンド '{command}' の実行結果: {result[:100]}...")
            
            return result
        except Exception as e:
            error_msg = f"コマンド実行エラー: {str(e)}"
            logger.error(error_msg, exc_info=True)
            return error_msg
    
    async def close(self):
        """MCPクライアントを閉じる"""
        if self.client:
            try:
                await self.client.close()
                logger.info("MCPクライアントを閉じました")
            except Exception as e:
                logger.error(f"MCPクライアントを閉じる際にエラー: {str(e)}")

# MCPコントローラのグローバルインスタンス
mcp_terminal_controller = MCPTerminalController()

def set_bot_status(
    status_text="起動中！", status_emoji=":robot_face:", presence="auto"
):
    """
    ボットのステータスを設定する

    Args:
        status_text: ステータスメッセージ
        status_emoji: ステータス絵文字
        presence: プレゼンス状態（"auto" または "away"）
    """
    if not SLACK_BOT_TOKEN:
        logger.warning(
            "SLACK_BOT_TOKENが設定されていないため、ステータスを設定できません"
        )
        return

    try:
        client = WebClient(token=SLACK_BOT_TOKEN)

        # プレゼンス（オンライン状態）を設定
        try:
            client.users_setPresence(presence=presence)
        except Exception as presence_error:
            logger.warning(f"プレゼンス設定中にエラー: {str(presence_error)}")

        # ステータスを設定
        try:
            client.users_profile_set(
                profile={
                    "status_text": status_text,
                    "status_emoji": status_emoji,
                    "status_expiration": 0,
                }
            )
            logger.info(f"ボットステータスを設定: {status_text}")
        except Exception as profile_error:
            # ステータス設定権限がない場合はエラーを記録するだけで続行
            logger.warning(f"プロフィール設定中にエラー: {str(profile_error)}")

    except Exception as e:
        logger.error(f"ボットステータス設定中にエラー: {str(e)}")


# グローバル変数としてボットのスレッドを保持
_bot_thread = None
_bot_exit_event = None


def start_slack_bot_in_background():
    """
    CLIからSlackボットをバックグラウンドスレッドで起動する
    """
    import threading

    global _bot_thread, _bot_exit_event

    # すでに起動している場合は何もしない
    if _bot_thread is not None and _bot_thread.is_alive():
        logger.info("Slackボットはすでに起動しています")
        return True

    if not SLACK_BOT_TOKEN or not SLACK_APP_TOKEN:
        logger.warning("Slackボットの起動に必要な環境変数が設定されていません")
        return False

    logger.info("Slackボットをバックグラウンドで起動します...")

    # スレッド終了用のイベント
    _bot_exit_event = threading.Event()

    def run_bot():
        try:
            # ボットを起動したらステータスを「起動中！」に設定
            set_bot_status(
                status_text="起動中！", status_emoji=":robot_face:", presence="auto"
            )

            handler = SocketModeHandler(app, SLACK_APP_TOKEN)
            logger.info("Socket Modeハンドラを作成しました")

            # 終了イベントが設定されるまで実行
            handler.connect()
            while not _bot_exit_event.is_set():
                try:
                    handler.client.process_message()
                except Exception as e:
                    if "interpreter shutdown" in str(e):
                        logger.info(
                            "インタープリタシャットダウンのため処理を停止します"
                        )
                        break
                    logger.error(f"メッセージ処理中のエラー: {str(e)}")
                    # 短い待機を入れてCPU使用率を抑える
                    time.sleep(0.1)

        except Exception as e:
            logger.error(f"バックグラウンドでのSlackボット起動に失敗: {str(e)}")

    # バックグラウンドスレッドでボットを起動
    _bot_thread = threading.Thread(target=run_bot, daemon=True)
    _bot_thread.start()
    logger.info("Slackボットをバックグラウンドで起動しました")

    # 終了時にクリーンアップを行うための関数をregister
    def cleanup_slack_bot():
        if _bot_exit_event:
            logger.info("Slackボットを終了しています...")
            _bot_exit_event.set()

            # 少し待ってからスレッドに終了を伝える
            if _bot_thread and _bot_thread.is_alive():
                _bot_thread.join(timeout=1.0)

    atexit.register(cleanup_slack_bot)

    return True


# Canvasメモ帳に関するスラッシュコマンドハンドラ
@app.command("/memo")
def handle_memo_command(ack, body, logger, say):
    """
    /memo スラッシュコマンドを処理する

    Args:
        ack: 受信確認
        body: リクエストボディ
        logger: ロガー
        say: メッセージ送信関数
    """
    # コマンドを確認応答
    ack()

    logger.info(f"スラッシュコマンド /memo を受信: {body}")

    # ユーザーIDを取得
    user_id = body["user_id"]

    # チャンネルIDを取得
    channel_id = body["channel_id"]

    # スレッドタイムスタンプを取得（もし存在すれば）
    thread_ts = body.get("thread_ts")

    # コマンドテキストを取得
    text = body.get("text", "").strip()

    # コマンド引数をパース
    args = text.split()

    # AIインスタンスを取得
    ai = get_ai_for_user(user_id)

    # サブコマンドの処理
    if len(args) > 0 and args[0] in ["list", "リスト", "一覧"]:
        # Canvas一覧表示
        list_canvases_for_user(user_id, channel_id, thread_ts, say)
    elif len(args) > 0 and args[0] in ["update", "更新", "編集"]:
        # 既存Canvas更新
        if len(args) < 2:
            say(
                text="使用法: /memo update <キャンバスID> [内容]",
                thread_ts=thread_ts,
                channel=channel_id,
            )
        else:
            canvas_id = args[1]
            content = " ".join(args[2:]) if len(args) > 2 else ""
            update_canvas_for_user(
                user_id, channel_id, canvas_id, content, thread_ts, say
            )
    elif len(args) > 0 and args[0] in ["history", "履歴"]:
        # Canvas履歴表示
        if len(args) < 2:
            say(
                text="使用法: /memo history <キャンバスID>",
                thread_ts=thread_ts,
                channel=channel_id,
            )
        else:
            canvas_id = args[1]
            show_canvas_history(user_id, channel_id, canvas_id, thread_ts, say)
    else:
        # 新規Canvas作成
        create_canvas_for_user(user_id, channel_id, text, thread_ts, say)


def create_canvas_for_user(user_id, channel_id, title_text, thread_ts, say):
    """
    ユーザー用のCanvasを作成する

    Args:
        user_id: Slackユーザー ID
        channel_id: チャンネルID
        title_text: タイトルテキスト（空の場合はデフォルトを使用）
        thread_ts: スレッドタイムスタンプ
        say: 応答関数
    """
    from slack_sdk.web import WebClient

    client = WebClient(token=SLACK_BOT_TOKEN)

    # タイトルが空の場合のデフォルト
    title = title_text if title_text else "共同編集メモ帳"

    try:
        # Canvasを作成
        canvas_response = client.conversations_canvases_create(
            channel_id=channel_id,
            document_content={
                "type": "markdown",
                "markdown": f"# {title}\n\n## メモの内容\n\nここに内容を入力してください。AIが必要に応じてサポートします。\n\n## AIへの指示\n\nここにAIへの指示を入力してください。",
            },
        )

        if canvas_response and canvas_response.get("ok"):
            canvas_id = canvas_response.get("canvas", {}).get("id")
            canvas_url = canvas_response.get("canvas", {}).get("permalink")

            # 成功メッセージを送信
            say(
                text=f"*共同編集メモ帳を作成しました*\n\n<{canvas_url}|メモ帳を開く>\n\nAIに指示するには、メモ帳の「AIへの指示」セクションに内容を入力してください。",
                thread_ts=thread_ts,
                channel=channel_id,
            )

            # キャンバスIDをユーザーのAIに保存
            ai = get_ai_for_user(user_id)
            ai.remember(f"canvas_{canvas_id}", f"ユーザーの共同編集メモ帳: {title}")

            logger.info(f"ユーザー {user_id} にCanvas {canvas_id} を作成しました")
        else:
            error_message = canvas_response.get("error", "不明なエラー")
            logger.error(f"Canvas作成エラー: {error_message}")
            say(
                text=f"メモ帳の作成に失敗しました: {error_message}",
                thread_ts=thread_ts,
                channel=channel_id,
            )
    except Exception as e:
        logger.error(f"Canvas作成中に例外が発生: {str(e)}", exc_info=True)
        say(
            text=f"メモ帳の作成中にエラーが発生しました: {str(e)}",
            thread_ts=thread_ts,
            channel=channel_id,
        )


def list_canvases_for_user(user_id, channel_id, thread_ts, say):
    """
    ユーザーのCanvas一覧を表示する

    Args:
        user_id: Slackユーザー ID
        channel_id: チャンネルID
        thread_ts: スレッドタイムスタンプ
        say: 応答関数
    """
    from slack_sdk.web import WebClient

    client = WebClient(token=SLACK_BOT_TOKEN)

    try:
        # Canvasの一覧を取得 (files.listメソッドをtypes=canvasフィルタで使用)
        files_response = client.files_list(types="canvas", channel=channel_id)

        if files_response and files_response.get("ok"):
            canvases = files_response.get("files", [])

            if canvases:
                # Canvas一覧の表示形式を整形
                canvas_list = []
                for canvas in canvases:
                    canvas_id = canvas.get("id")
                    canvas_title = canvas.get("title", "無題")
                    canvas_url = canvas.get("permalink")
                    canvas_created = datetime.fromtimestamp(
                        float(canvas.get("created", 0))
                    ).strftime("%Y-%m-%d %H:%M")

                    canvas_list.append(
                        f"• <{canvas_url}|{canvas_title}> (ID: {canvas_id}, 作成: {canvas_created})"
                    )

                canvas_list_text = "\n".join(canvas_list)
                say(
                    text=f"*利用可能なメモ帳一覧*\n\n{canvas_list_text}\n\n更新するには: `/memo update <キャンバスID> [内容]`\n履歴を見るには: `/memo history <キャンバスID>`",
                    thread_ts=thread_ts,
                    channel=channel_id,
                )
            else:
                say(
                    text="このチャンネルに共有されているメモ帳はありません。\n\n新しいメモ帳を作成するには: `/memo [タイトル]`",
                    thread_ts=thread_ts,
                    channel=channel_id,
                )
        else:
            error_message = files_response.get("error", "不明なエラー")
            logger.error(f"Canvas一覧取得エラー: {error_message}")
            say(
                text=f"メモ帳一覧の取得に失敗しました: {error_message}",
                thread_ts=thread_ts,
                channel=channel_id,
            )
    except Exception as e:
        logger.error(f"Canvas一覧取得中に例外が発生: {str(e)}", exc_info=True)
        say(
            text=f"メモ帳一覧の取得中にエラーが発生しました: {str(e)}",
            thread_ts=thread_ts,
            channel=channel_id,
        )


def update_canvas_for_user(user_id, channel_id, canvas_id, content, thread_ts, say):
    """
    ユーザーのCanvasを更新する

    Args:
        user_id: Slackユーザー ID
        channel_id: チャンネルID
        canvas_id: 更新対象のCanvasのID
        content: 追加する内容
        thread_ts: スレッドタイムスタンプ
        say: 応答関数
    """
    from slack_sdk.web import WebClient

    client = WebClient(token=SLACK_BOT_TOKEN)

    try:
        # 現在のAIインスタンスからの情報取得
        ai = get_ai_for_user(user_id)

        # Canvas情報を取得
        canvas_response = client.canvases_get(canvas_id=canvas_id)

        if canvas_response and canvas_response.get("ok"):
            # 内容が空の場合は、AIに応答を生成させる
            if not content:
                # Canvas内容を取得
                canvas_content = (
                    canvas_response.get("canvas", {})
                    .get("document_content", {})
                    .get("markdown", "")
                )

                # AIからの回答を生成
                ai_response = ai.chat(
                    f"以下はSlackのCanvasで共有されているメモ帳の内容です。この内容に対して補足や追加をしてください。:\n\n{canvas_content}"
                )

                # 更新用の内容を設定
                content = ai_response

            # ユーザー名を取得
            user_info = client.users_info(user=user_id)
            user_name = user_info.get("user", {}).get("real_name", "ユーザー")

            # Canvas更新用の内容を整形
            formatted_content = f"\n\n## {user_name}の追加 ({datetime.now().strftime('%Y-%m-%d %H:%M')})\n\n{content}"

            # Canvasを更新
            update_response = client.canvases_edit(
                canvas_id=canvas_id,
                changes=[
                    {
                        "operation": "append",
                        "document_content": {
                            "type": "markdown",
                            "markdown": formatted_content,
                        },
                    }
                ],
            )

            if update_response and update_response.get("ok"):
                canvas_url = canvas_response.get("canvas", {}).get("permalink")
                say(
                    text=f"*メモ帳を更新しました*\n\n<{canvas_url}|メモ帳を開く>",
                    thread_ts=thread_ts,
                    channel=channel_id,
                )

                logger.info(f"ユーザー {user_id} がCanvas {canvas_id} を更新しました")
            else:
                error_message = update_response.get("error", "不明なエラー")
                logger.error(f"Canvas更新エラー: {error_message}")
                say(
                    text=f"メモ帳の更新に失敗しました: {error_message}",
                    thread_ts=thread_ts,
                    channel=channel_id,
                )
        else:
            error_message = canvas_response.get("error", "不明なエラー")
            logger.error(f"Canvas取得エラー: {error_message}")
            say(
                text=f"メモ帳の取得に失敗しました: {error_message}",
                thread_ts=thread_ts,
                channel=channel_id,
            )
    except Exception as e:
        logger.error(f"Canvas更新中に例外が発生: {str(e)}", exc_info=True)
        say(
            text=f"メモ帳の更新中にエラーが発生しました: {str(e)}",
            thread_ts=thread_ts,
            channel=channel_id,
        )


def show_canvas_history(user_id, channel_id, canvas_id, thread_ts, say):
    """
    Canvas編集履歴を表示する

    Args:
        user_id: Slackユーザー ID
        channel_id: チャンネルID
        canvas_id: 対象のCanvasのID
        thread_ts: スレッドタイムスタンプ
        say: 応答関数
    """
    from slack_sdk.web import WebClient

    client = WebClient(token=SLACK_BOT_TOKEN)

    try:
        # Canvas情報を取得
        canvas_response = client.canvases_get(canvas_id=canvas_id)

        if canvas_response and canvas_response.get("ok"):
            canvas_title = canvas_response.get("canvas", {}).get("title", "無題")
            canvas_content = (
                canvas_response.get("canvas", {})
                .get("document_content", {})
                .get("markdown", "")
            )
            canvas_url = canvas_response.get("canvas", {}).get("permalink")

            # 編集履歴を抽出 (##で始まるセクションを検索)
            history_sections = []
            lines = canvas_content.split("\n")

            current_section = None
            for line in lines:
                if line.startswith("## ") and ("追加" in line or "回答" in line):
                    # 新しいセクションの開始
                    if current_section:
                        history_sections.append(current_section)
                    current_section = {"heading": line, "content": []}
                elif current_section:
                    # 現在のセクションに内容を追加
                    current_section["content"].append(line)

            # 最後のセクションを追加
            if current_section:
                history_sections.append(current_section)

            # 履歴を整形
            if history_sections:
                history_text = []
                for section in history_sections:
                    heading = section["heading"].replace("##", "").strip()
                    content_preview = "\n".join(section["content"][:3])
                    if len(section["content"]) > 3:
                        content_preview += "\n..."
                    history_text.append(f"• *{heading}*\n```\n{content_preview}\n```")

                history_formatted = "\n\n".join(history_text)
                say(
                    text=f"*メモ帳「{canvas_title}」の編集履歴*\n\n{history_formatted}\n\n<{canvas_url}|メモ帳を開く>",
                    thread_ts=thread_ts,
                    channel=channel_id,
                )
            else:
                say(
                    text=f"*メモ帳「{canvas_title}」の編集履歴*\n\nこのメモ帳にはまだ編集履歴がありません。\n\n<{canvas_url}|メモ帳を開く>",
                    thread_ts=thread_ts,
                    channel=channel_id,
                )
        else:
            error_message = canvas_response.get("error", "不明なエラー")
            logger.error(f"Canvas取得エラー: {error_message}")
            say(
                text=f"メモ帳の取得に失敗しました: {error_message}",
                thread_ts=thread_ts,
                channel=channel_id,
            )
    except Exception as e:
        logger.error(f"Canvas履歴取得中に例外が発生: {str(e)}", exc_info=True)
        say(
            text=f"メモ帳の履歴取得中にエラーが発生しました: {str(e)}",
            thread_ts=thread_ts,
            channel=channel_id,
        )


@app.event("canvas_mention")
def handle_canvas_mention(body, logger, say):
    """
    Canvasでのメンションイベントを処理する

    Args:
        body: イベントボディ
        logger: ロガー
        say: メッセージ送信関数
    """
    try:
        event = body["event"]
        user_id = event["user"]
        canvas_id = event.get("canvas", {}).get("id")
        canvas_text = event.get("text", "")

        logger.info(
            f"Canvas内でのメンション検出: ユーザー {user_id}, キャンバス {canvas_id}"
        )

        # AIインスタンスを取得
        ai = get_ai_for_user(user_id)

        # Canvas内容を取得
        from slack_sdk.web import WebClient

        client = WebClient(token=SLACK_BOT_TOKEN)

        canvas_response = client.canvases_get(canvas_id=canvas_id)

        if canvas_response and canvas_response.get("ok"):
            canvas_content = (
                canvas_response.get("canvas", {})
                .get("document_content", {})
                .get("markdown", "")
            )

            # AIに指示セクションを検索
            instruction_section = None
            content_sections = canvas_content.split("## AIへの指示")

            if len(content_sections) > 1:
                instruction_section = content_sections[1].strip()

            if instruction_section:
                # AIからの回答を生成
                ai_response = ai.chat(
                    f"以下はSlackのCanvasで共有されているメモ帳の内容です。'AIへの指示'セクションに書かれた内容に対して応答してください。メモ全体：\n\n{canvas_content}"
                )

                # Canvas更新用の回答を整形
                formatted_response = f"\n\n## AIからの回答 ({datetime.now().strftime('%Y-%m-%d %H:%M')})\n\n{ai_response}"

                # Canvasを更新
                update_response = client.canvases_edit(
                    canvas_id=canvas_id,
                    changes=[
                        {
                            "operation": "append",
                            "document_content": {
                                "type": "markdown",
                                "markdown": formatted_response,
                            },
                        }
                    ],
                )

                if not update_response or not update_response.get("ok"):
                    error_message = update_response.get("error", "不明なエラー")
                    logger.error(f"Canvas更新エラー: {error_message}")
            else:
                logger.warning(
                    f"AIへの指示セクションが見つかりませんでした: {canvas_id}"
                )
        else:
            logger.error(
                f"Canvasの取得に失敗: {canvas_response.get('error', '不明なエラー')}"
            )

    except Exception as e:
        logger.error(f"Canvas処理中に例外が発生: {str(e)}", exc_info=True)


# スラッシュコマンドハンドラ
@app.command("/link")
def handle_link_command(ack, body, logger, say):
    """
    /link スラッシュコマンドを処理する

    Args:
        ack: 受信確認
        body: リクエストボディ
        logger: ロガー
        say: メッセージ送信関数
    """
    # コマンドを確認応答
    ack()

    logger.info(f"スラッシュコマンド /link を受信: {body}")

    # ユーザーIDを取得
    user_id = body["user_id"]

    # チャンネルIDを取得
    channel_id = body["channel_id"]

    # スレッドタイムスタンプを取得（もし存在すれば）
    thread_ts = body.get("thread_ts")

    # AIインスタンスを取得
    ai = get_ai_for_user(user_id)

    # リンクコードを生成
    link_code = user_link_manager.generate_link_code(user_id)

    # コマンドラインツール用の使い方を表示
    cli_command = f"personal-ai link {link_code}"

    # プライベートレスポンスとして送信（他のユーザーには見えない）
    say(
        text=f"*CLIとSlackアカウントのリンク*\n\nCLIアプリケーションで次のコマンドを実行してリンクを完了してください:\n```\n{cli_command}\n```\n\nこのリンクコードは現在無期限で有効です（テスト中）。",
        thread_ts=thread_ts,
        channel=channel_id,
        user=user_id,
        response_type="ephemeral",  # プライベートレスポンス
    )

    logger.info(f"ユーザー {user_id} にリンクコード {link_code} を生成しました")


@app.command("/terminal")
def handle_terminal_command(ack, body, logger, say):
    """
    /terminal スラッシュコマンドを処理する - MCPを使用してターミナルコマンドを実行

    Args:
        ack: 受信確認
        body: リクエストボディ
        logger: ロガー
        say: メッセージ送信関数
    """
    # コマンドを確認応答
    ack()

    logger.info(f"スラッシュコマンド /terminal を受信: {body}")

    # ユーザーIDを取得
    user_id = body["user_id"]

    # チャンネルIDを取得
    channel_id = body["channel_id"]

    # スレッドタイムスタンプを取得（もし存在すれば）
    thread_ts = body.get("thread_ts")

    # コマンドテキストを取得
    command_text = body.get("text", "").strip()

    if not command_text:
        say(
            text="使用法: /terminal <実行するコマンド>",
            thread_ts=thread_ts,
            channel=channel_id,
        )
        return

    # 処理中メッセージを送信
    say(
        text=f"コマンド `{command_text}` を実行中...",
        thread_ts=thread_ts,
        channel=channel_id,
    )

    # MCPが利用可能か確認
    if not MCP_AVAILABLE:
        say(
            text="MCPが利用できません。以下のパッケージをインストールしてください:\n```\npip install mcp langchain-mcp-adapters\nnpm install -g @joshuarileydev/terminal-mcp-server\n```",
            thread_ts=thread_ts,
            channel=channel_id,
        )
        return

    # 非同期関数でMCPコマンドを実行
    async def run_mcp_command():
        try:
            # コマンド実行
            result = await mcp_terminal_controller.execute_command(command_text)
            
            # 結果が長すぎる場合は切り詰める
            if len(result) > 3000:
                result = result[:3000] + "\n... (結果が長すぎるため切り詰められました)"
            
            # 結果を送信
            say(
                text=f"コマンド `{command_text}` の実行結果:\n```\n{result}\n```",
                thread_ts=thread_ts,
                channel=channel_id,
            )
            
            # ユーザーのAIインスタンスを取得
            ai = get_ai_for_user(user_id)
            
            # 対話履歴に保存
            ai.save_conversation(
                f"[ターミナルコマンド] {command_text}", result, None  # 新しい会話IDを生成
            )
            
        except Exception as e:
            error_msg = f"コマンド実行中にエラーが発生しました: {str(e)}"
            logger.error(error_msg, exc_info=True)
            say(
                text=error_msg,
                thread_ts=thread_ts,
                channel=channel_id,
            )

    # 非同期関数を実行
    loop = asyncio.new_event_loop()
    asyncio.set_event_loop(loop)
    loop.run_until_complete(run_mcp_command())
    loop.close()


# ユーザーリンク管理クラス
class UserLinkManager:
    """
    異なるプラットフォーム（SlackとCLI）間のユーザーリンクを管理するクラス
    """

    def __init__(self, db_path: Optional[str] = None):
        """
        UserLinkManagerを初期化

        Args:
            db_path: リンク情報を保存するDBファイルへのパス
        """
        if db_path is None:
            # デフォルトのDBパスはユーザーのホームディレクトリ
            home_dir = os.path.expanduser("~")
            data_dir = os.path.join(home_dir, ".personal_ai")
            os.makedirs(data_dir, exist_ok=True)
            db_path = os.path.join(data_dir, "user_links.db")

        self.db_path = db_path
        self._init_db()
        logger.info(f"ユーザーリンクマネージャーを初期化しました: {db_path}")

    def _init_db(self):
        """SQLiteデータベースを初期化"""
        try:
            conn = sqlite3.connect(self.db_path)
            cursor = conn.cursor()

            # ユーザーリンクテーブルを作成
            cursor.execute(
                """
            CREATE TABLE IF NOT EXISTS user_links (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                slack_user_id TEXT UNIQUE,
                cli_user_id TEXT,
                link_code TEXT UNIQUE,
                created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                verified BOOLEAN DEFAULT 0
            )
            """
            )

            conn.commit()
            conn.close()
            logger.debug("ユーザーリンクデータベースを初期化しました")
        except Exception as e:
            logger.error(f"データベース初期化中にエラー: {str(e)}", exc_info=True)

    def generate_link_code(self, slack_user_id: str) -> str:
        """
        Slackユーザー用のリンクコードを生成

        Args:
            slack_user_id: SlackユーザーID

        Returns:
            生成されたリンクコード
        """
        try:
            # ランダムなリンクコードを生成
            random_bytes = os.urandom(6)  # 6バイト = 12文字の16進数
            link_code = hashlib.sha256(
                random_bytes + slack_user_id.encode()
            ).hexdigest()[:12]

            # データベースに保存
            conn = sqlite3.connect(self.db_path)
            cursor = conn.cursor()

            # 既存のエントリがあるか確認
            cursor.execute(
                "SELECT id FROM user_links WHERE slack_user_id = ?", (slack_user_id,)
            )
            existing = cursor.fetchone()

            if existing:
                # 既存のエントリを更新
                cursor.execute(
                    "UPDATE user_links SET link_code = ?, verified = 0 WHERE slack_user_id = ?",
                    (link_code, slack_user_id),
                )
            else:
                # 新しいエントリを作成
                cursor.execute(
                    "INSERT INTO user_links (slack_user_id, link_code) VALUES (?, ?)",
                    (slack_user_id, link_code),
                )

            conn.commit()
            conn.close()
            logger.info(f"リンクコードを生成しました: {slack_user_id} -> {link_code}")
            return link_code

        except Exception as e:
            logger.error(f"リンクコード生成中にエラー: {str(e)}", exc_info=True)
            return "エラー"

    def verify_link(self, link_code: str, cli_user_id: str) -> bool:
        """
        CLIユーザーがリンクコードを検証

        Args:
            link_code: 検証するリンクコード
            cli_user_id: CLIユーザーID

        Returns:
            検証成功かどうか
        """
        try:
            conn = sqlite3.connect(self.db_path)
            cursor = conn.cursor()

            # リンクコードを検索
            cursor.execute(
                "SELECT slack_user_id FROM user_links WHERE link_code = ?", (link_code,)
            )
            result = cursor.fetchone()

            if not result:
                logger.warning(f"無効なリンクコード: {link_code}")
                conn.close()
                return False

            slack_user_id = result[0]

            # リンクを更新
            cursor.execute(
                "UPDATE user_links SET cli_user_id = ?, verified = 1 WHERE link_code = ?",
                (cli_user_id, link_code),
            )

            conn.commit()
            conn.close()

            logger.info(f"リンク検証成功: CLI {cli_user_id} ⟷ Slack {slack_user_id}")
            return True

        except Exception as e:
            logger.error(f"リンク検証中にエラー: {str(e)}", exc_info=True)
            return False

    def get_linked_user_id(self, platform_user_id: str, platform: str) -> Optional[str]:
        """
        紐づけられたユーザーIDを取得

        Args:
            platform_user_id: プラットフォーム上のユーザーID
            platform: プラットフォーム種類（'slack'または'cli'）

        Returns:
            紐づけられたユーザーID、または未リンク時はNone
        """
        try:
            conn = sqlite3.connect(self.db_path)
            cursor = conn.cursor()

            if platform == "slack":
                # SlackからCLIへのリンクを検索
                cursor.execute(
                    "SELECT cli_user_id, verified FROM user_links WHERE slack_user_id = ?",
                    (platform_user_id,),
                )
            else:
                # CLIからSlackへのリンクを検索
                cursor.execute(
                    "SELECT slack_user_id, verified FROM user_links WHERE cli_user_id = ?",
                    (platform_user_id,),
                )

            result = cursor.fetchone()
            conn.close()

            if result and result[0] and result[1]:  # ユーザーIDが存在し、検証済み
                linked_id = result[0]
                logger.debug(
                    f"リンクされたユーザーID: {platform} {platform_user_id} -> {linked_id}"
                )
                return linked_id

            return None

        except Exception as e:
            logger.error(
                f"リンクされたユーザーID取得中にエラー: {str(e)}", exc_info=True
            )
            return None

    def get_all_links(self) -> List[Dict[str, Any]]:
        """
        すべてのユーザーリンクを取得

        Returns:
            リンク情報のリスト
        """
        try:
            conn = sqlite3.connect(self.db_path)
            cursor = conn.cursor()

            cursor.execute(
                "SELECT slack_user_id, cli_user_id, link_code, created_at, verified FROM user_links"
            )
            rows = cursor.fetchall()
            conn.close()

            links = []
            for row in rows:
                links.append(
                    {
                        "slack_user_id": row[0],
                        "cli_user_id": row[1],
                        "link_code": row[2],
                        "created_at": row[3],
                        "verified": bool(row[4]),
                    }
                )

            return links

        except Exception as e:
            logger.error(f"全リンク取得中にエラー: {str(e)}", exc_info=True)
            return []


# ユーザーリンクマネージャーのインスタンス
user_link_manager = UserLinkManager()

# ユーザーごとのAIインスタンスを保持する辞書
user_ai_instances: Dict[str, PersonalAI] = {}


def get_ai_for_user(user_id: str) -> PersonalAI:
    """
    ユーザーIDに対応するAIインスタンスを取得または作成

    Args:
        user_id: SlackのユーザーID

    Returns:
        ユーザー専用のPersonalAIインスタンス
    """
    if user_id not in user_ai_instances:
        # リンクされたCLIユーザーIDがあるか確認
        cli_user_id = user_link_manager.get_linked_user_id(user_id, "slack")

        # リンクされたCLIユーザーIDがあれば、そのIDを使用
        actual_user_id = f"cli_{cli_user_id}" if cli_user_id else f"slack_{user_id}"

        # 新しいAIインスタンスを作成
        logger.info(
            f"ユーザー {user_id} の新しいAIインスタンスを作成します (モデル: {OLLAMA_MODEL}, 使用ID: {actual_user_id})"
        )
        user_ai_instances[user_id] = PersonalAI(
            user_id=actual_user_id, ollama_model=OLLAMA_MODEL
        )
        logger.info(f"ユーザー {user_id} のAIインスタンス作成完了")
    else:
        logger.debug(f"ユーザー {user_id} の既存AIインスタンスを使用します")

    return user_ai_instances[user_id]


def parse_command(text: str) -> Optional[Dict[str, Any]]:
    """
    テキストからコマンドを解析

    Args:
        text: メッセージテキスト

    Returns:
        コマンド情報のディクショナリまたはNone
    """
    command_pattern = r"^/(\w+)(?:\s+(.*))?$"
    match = re.match(command_pattern, text)

    if match:
        command = match.group(1)
        args_str = match.group(2) or ""
        args = args_str.split() if args_str else []

        logger.debug(f"コマンドを検出: {command}, 引数: {args}")
        return {"command": command, "args": args, "full_args": args_str}

    return None


def format_command_result(result: str) -> str:
    """
    コマンド結果をSlack用に整形

    Args:
        result: コマンド実行結果

    Returns:
        整形されたテキスト
    """
    # Markdownヘッダをボールド変換
    result = re.sub(r"^# (.*?)$", r"*\1*", result, flags=re.MULTILINE)
    result = re.sub(r"^## (.*?)$", r"*\1*", result, flags=re.MULTILINE)

    # リストをSlack用に変換
    result = re.sub(r"^- ", r"• ", result, flags=re.MULTILINE)

    return result


def handle_help_command(ai: PersonalAI) -> str:
    """
    ヘルプメッセージを生成する

    Args:
        ai: PersonalAIインスタンス

    Returns:
        ヘルプメッセージ
    """
    commands = [
        "*基本コマンド*",
        "• `/help` または `!help` - このヘルプを表示",
        "• `/remember <キー> <値>` - 情報を記憶する",
        "• `/forget <キー>` - 情報を忘れる",
        "• `/profile` - あなたの記憶済み情報を表示",
        "• `/search <検索語>` - 記憶や会話履歴を検索",
        "• `/settings` - 設定を表示",
        "• `/continue` - 会話を継続",
        "",
        "*機能コマンド*",
        "• `/image list` - 保存された画像の一覧を表示",
        "• `/image show <ID>` - 指定したIDの画像を表示",
        "• `/image search <キーワード>` - 画像をキーワードで検索",
        "",
        "*メモ帳機能*",
        "• `/memo <タイトル>` - 指定したタイトルの共同編集メモ帳を作成",
        "• `/memo list` - 現在のチャンネルで利用可能なメモ帳一覧を表示",
        "• `/memo update <ID> <内容>` - 既存のメモ帳を更新（内容省略時はAIが自動で補足）",
        "• `/memo history <ID>` - メモ帳の編集履歴を表示",
        "",
        "*ターミナル機能*",
    ]

    if MCP_AVAILABLE:
        commands.append("• `/terminal <コマンド>` - ターミナルコマンドを実行")
        commands.append("• `$ <コマンド>` - 頭に $ をつけてコマンドを実行")
        commands.append("• `実行: <コマンド>` - 「実行:」の後にコマンドを記述して実行")
        commands.append("• `ターミナル: <コマンド>` - 「ターミナル:」の後にコマンドを記述して実行")
        commands.append("• `コマンド: <コマンド>` - 「コマンド:」の後にコマンドを記述して実行")
    else:
        commands.append("• ターミナル機能を使用するには追加パッケージのインストールが必要です:")
        commands.append("  `pip install mcp langchain-mcp-adapters`")
        commands.append("  `npm install -g @joshuarileydev/terminal-mcp-server`")

    commands.append("")
    commands.append("*高度な機能*")

    if BROWSER_USE_AVAILABLE:
        commands.append("• `/browser <タスク>` - ウェブブラウザを使ったタスクを実行")

    commands.append("")
    commands.append("通常の会話では、質問や指示を直接入力するだけで応答します。")
    commands.append("")
    commands.append("*ファイル対応*")
    commands.append("• 画像: 画像ファイルを添付すると内容の説明や分析を行います")

    if PDF_SUPPORT:
        commands.append(
            "• PDF: PDFファイルを添付するとテキスト抽出して内容を分析します"
        )

    commands.append("• CSV: CSVファイルを添付するとデータ分析を行います")
    commands.append("")
    commands.append(
        "より詳しいヘルプが必要な場合は、「コマンドの詳しい使い方を教えて」と質問してください。"
    )

    return "\n".join(commands)


def download_file(url: str, token: str, filename: str = None) -> Tuple[str, str]:
    """
    Slackからファイルをダウンロードして一時ファイルに保存

    Args:
        url: ファイルのURL
        token: SlackのBotトークン
        filename: 元のファイル名（デバッグ用）

    Returns:
        (一時ファイルのパス, MIMEタイプ)
    """
    headers = {"Authorization": f"Bearer {token}"}
    max_retries = 3
    timeout = (10, 30)  # 接続タイムアウト10秒、読み込みタイムアウト30秒

    for attempt in range(max_retries):
        try:
            logger.info(
                f"ファイルをダウンロード中 (試行 {attempt+1}/{max_retries}): {url}"
            )

            # ストリーミングでデータを取得して整合性を確保
            with requests.get(
                url, headers=headers, stream=True, timeout=timeout
            ) as response:
                response.raise_for_status()

                # MIMEタイプとサイズを取得
                content_type = response.headers.get("Content-Type", "")
                content_length = int(response.headers.get("Content-Length", 0))
                logger.debug(
                    f"ファイルヘッダー情報: Content-Type={content_type}, Content-Length={content_length}バイト"
                )

                # 拡張子を決定
                ext = ".bin"  # デフォルト
                if "png" in content_type:
                    ext = ".png"
                elif "jpeg" in content_type or "jpg" in content_type:
                    ext = ".jpg"
                elif "gif" in content_type:
                    ext = ".gif"
                elif "csv" in content_type or "text/csv" in content_type:
                    ext = ".csv"
                elif "pdf" in content_type:
                    ext = ".pdf"
                elif "text/plain" in content_type:
                    ext = ".txt"
                elif filename:
                    # ファイル名から拡張子を取得
                    name_parts = filename.split(".")
                    if len(name_parts) > 1:
                        ext = f".{name_parts[-1].lower()}"

                # ファイルパスを決定
                if DEBUG_MODE:
                    # デバッグモードの場合、ファイルを一定の場所に保存
                    os.makedirs(DEBUG_IMAGES_DIR, exist_ok=True)
                    debug_filename = (
                        f"{int(datetime.now().timestamp())}_{filename or 'file'}{ext}"
                    )
                    file_path = os.path.join(DEBUG_IMAGES_DIR, debug_filename)
                else:
                    # 通常モードでは一時ディレクトリに保存
                    temp_dir = tempfile.gettempdir()
                    temp_filename = f"slack_file_{int(datetime.now().timestamp())}_{filename or 'file'}{ext}"
                    file_path = os.path.join(temp_dir, temp_filename)

                # データをチャンクで取得して保存し、ファイルの整合性を確保
                downloaded_size = 0
                with open(file_path, "wb") as f:
                    for chunk in response.iter_content(chunk_size=8192):
                        if chunk:  # フィルタで空のチャンクをスキップ
                            f.write(chunk)
                            downloaded_size += len(chunk)

                # ダウンロードサイズの確認
                if content_length > 0 and downloaded_size != content_length:
                    logger.warning(
                        f"ダウンロードサイズの不一致: 予定={content_length}, 実際={downloaded_size}"
                    )
                    # 不一致の場合は再試行する可能性あり
                    if attempt < max_retries - 1:
                        continue

                logger.info(
                    f"ファイルダウンロード完了: パス={file_path}, サイズ={downloaded_size}バイト"
                )
                return file_path, content_type

        except (requests.RequestException, IOError) as e:
            logger.error(f"ファイルダウンロード試行 {attempt+1} が失敗: {str(e)}")
            if attempt >= max_retries - 1:
                logger.error("リトライ回数を超過しました。ダウンロードを諦めます。")
                return None, None
            # 一時的なエラーの場合、少し待機してリトライ
            time.sleep(1 * (attempt + 1))

    logger.error("すべての試行が失敗しました。ファイルをダウンロードできませんでした。")
    return None, None


def get_context_info() -> Dict[str, str]:
    """
    現在のコンテキスト情報（日時、曜日など）を取得する

    Returns:
        コンテキスト情報を含む辞書
    """
    now = datetime.now()

    # 日時情報
    current_date = now.strftime("%Y年%m月%d日")
    current_time = now.strftime("%H時%M分")
    current_datetime = f"{current_date} {current_time}"

    # 曜日
    weekdays = ["月曜日", "火曜日", "水曜日", "木曜日", "金曜日", "土曜日", "日曜日"]
    day_of_week = weekdays[now.weekday()]

    # 位置情報（可能であれば）
    location_info = "利用不可"
    try:
        import geocoder

        g = geocoder.ip("me")
        if g and g.ok and g.city:
            location_info = f"{g.city}, {g.state}, {g.country}"
    except Exception as e:
        logger.debug(f"位置情報の取得に失敗: {str(e)}")

    context = {
        "current_date": current_date,
        "current_time": current_time,
        "current_datetime": current_datetime,
        "day_of_week": day_of_week,
        "location": location_info,
    }

    logger.debug(f"コンテキスト情報: {context}")
    return context


def analyze_csv_file(file_path: str, prompt: str = None) -> str:
    """
    CSVファイルを解析して内容を要約・説明する

    Args:
        file_path: CSVファイルのパス
        prompt: 任意の追加プロンプト

    Returns:
        解析結果のテキスト
    """
    try:
        # CSVファイルの基本情報を収集
        file_size = os.path.getsize(file_path)
        logger.info(f"CSVファイル解析開始: {file_path}, サイズ: {file_size}バイト")

        # pandas DataFrameとして読み込み
        df = pd.read_csv(file_path)

        # CSV概要情報
        row_count = len(df)
        col_count = len(df.columns)
        column_names = ", ".join(df.columns.tolist())

        # サンプルデータ（最初の5行）を取得
        sample_data = df.head(5).to_string(index=False)

        # 数値列の基本統計情報
        stats_info = ""
        numeric_cols = df.select_dtypes(include=["number"]).columns
        if len(numeric_cols) > 0:
            stats_df = df[numeric_cols].describe().round(2)
            stats_info = f"\n\n数値データの基本統計:\n{stats_df.to_string()}"

        # CSVの情報をテキストにまとめる
        csv_info = f"""
CSVファイル概要:
- 行数: {row_count}
- 列数: {col_count} 
- カラム: {column_names}

サンプルデータ（最初の5行）:
{sample_data}
{stats_info}
"""

        # コンテキスト情報を取得
        context = get_context_info()

        # プロンプトが指定されている場合は、CSVの情報とプロンプトを使ってOllamaに問い合わせ
        if prompt:
            logger.info(f"CSVファイル解析: Ollamaに問い合わせ - {prompt}")

            # コンテキスト情報を含むシステムプロンプトを作成
            system_prompt = f"""あなたはCSVデータの解析と説明を行うアシスタントです。以下のCSVファイルの情報に基づいて質問に答えてください。

## システム情報
現在日時: {context['current_datetime']} ({context['day_of_week']})
現在位置: {context['location']}

## CSVデータ情報
{csv_info}
"""

            # LLMを使用して解析
            response = ollama.chat(
                model=OLLAMA_MODEL,
                messages=[
                    {
                        "role": "system",
                        "content": system_prompt,
                    },
                    {"role": "user", "content": prompt},
                ],
            )

            result = response["message"]["content"]
            return result
        else:
            # プロンプトがない場合はCSV情報のみを返す
            return f"CSVファイルの解析結果:\n\n{csv_info}\n\n解析日時: {context['current_datetime']} ({context['day_of_week']})"

    except Exception as e:
        logger.error(f"CSVファイル解析エラー: {str(e)}", exc_info=True)
        return f"CSVファイルの解析中にエラーが発生しました: {str(e)}"


def analyze_pdf_file(file_path: str, prompt: str = None) -> str:
    """
    PDFファイルを解析してテキスト内容を抽出・要約する

    Args:
        file_path: PDFファイルのパス
        prompt: 任意の追加プロンプト

    Returns:
        解析結果のテキスト
    """
    if not PDF_SUPPORT:
        return "PDFの解析にはPyPDF2ライブラリが必要です。'pip install PyPDF2'を実行してインストールしてください。"

    try:
        # PDFファイルの基本情報を収集
        file_size = os.path.getsize(file_path)
        logger.info(f"PDFファイル解析開始: {file_path}, サイズ: {file_size}バイト")

        # PDFからテキストを抽出
        text_content = ""
        with open(file_path, "rb") as pdf_file:
            pdf_reader = PyPDF2.PdfReader(pdf_file)
            page_count = len(pdf_reader.pages)

            # 最初の数ページのテキストを抽出（全ページだと多すぎる場合があるため）
            max_pages = min(5, page_count)
            for page_num in range(max_pages):
                page = pdf_reader.pages[page_num]
                text_content += page.extract_text() + "\n\n"

            # テキストが長すぎる場合は切り詰める
            if len(text_content) > 4000:
                text_content = text_content[:4000] + "...(省略)..."

        # コンテキスト情報を取得
        context = get_context_info()

        # PDF情報をまとめる
        pdf_info = f"""
PDFファイル概要:
- ページ数: {page_count}
- ファイルサイズ: {file_size} バイト

抽出されたテキスト（最初の数ページ）:
{text_content}
"""

        # プロンプトが指定されている場合は、PDF情報とプロンプトを使ってOllamaに問い合わせ
        if prompt:
            logger.info(f"PDFファイル解析: Ollamaに問い合わせ - {prompt}")

            # コンテキスト情報を含むシステムプロンプトを作成
            system_prompt = f"""あなたはPDFドキュメントの解析と要約を行うアシスタントです。以下のPDFの情報に基づいて質問に答えてください。

## システム情報
現在日時: {context['current_datetime']} ({context['day_of_week']})
現在位置: {context['location']}

## PDFドキュメント情報
{pdf_info}
"""

            # LLMを使用して解析
            response = ollama.chat(
                model=OLLAMA_MODEL,
                messages=[
                    {
                        "role": "system",
                        "content": system_prompt,
                    },
                    {"role": "user", "content": prompt},
                ],
            )

            result = response["message"]["content"]
            return result
        else:
            # プロンプトがない場合はPDF情報を簡潔に返す
            short_content = (
                text_content[:500] + "...(省略)..."
                if len(text_content) > 500
                else text_content
            )
            return f"PDFファイルの解析結果:\n\nページ数: {page_count}\n\n内容の一部:\n{short_content}\n\n解析日時: {context['current_datetime']} ({context['day_of_week']})"

    except Exception as e:
        logger.error(f"PDFファイル解析エラー: {str(e)}", exc_info=True)
        return f"PDFファイルの解析中にエラーが発生しました: {str(e)}"


def analyze_image_with_ollama(image_path: str, prompt: str) -> str:
    """
    画像をOllamaのマルチモーダルモデルを使用して解析する

    Args:
        image_path: 画像ファイルのパス
        prompt: 画像解析のプロンプト

    Returns:
        解析結果のテキスト
    """
    try:
        logger.info(f"画像解析: Ollamaマルチモーダルモデルを使用 - {prompt}")

        # コンテキスト情報を取得
        context = get_context_info()

        # 画像をbase64エンコード
        with open(image_path, "rb") as img_file:
            image_data = img_file.read()
            base64_image = base64.b64encode(image_data).decode("utf-8")

        # コンテキスト情報を含む拡張プロンプトを作成
        context_prompt = f"""現在の日時: {context['current_datetime']} ({context['day_of_week']})
現在の場所: {context['location']}

{prompt}"""

        # 使用するモデルを決定（デフォルトモデルが使用できない場合は代替モデルを試行）
        models_to_try = [IMAGE_MODEL] + FALLBACK_IMAGE_MODELS
        result = None
        last_error = None

        for model in models_to_try:
            try:
                logger.debug(f"画像解析に {model} モデルを試行中")

                # ollama.generate を使用して画像を解析
                response = ollama.generate(
                    model=model,
                    prompt=context_prompt,
                    images=[image_path],
                    options={"temperature": 0.7},
                )

                # 結果を取得
                if response and "response" in response:
                    result = response["response"]
                    logger.info(f"画像解析成功: モデル {model} を使用")
                    break
                else:
                    logger.warning(f"モデル {model} からの応答が不正: {response}")
                    continue

            except Exception as model_error:
                logger.warning(f"モデル {model} での画像解析に失敗: {str(model_error)}")
                last_error = model_error
                continue

        if result:
            return result
        else:
            error_msg = f"すべてのマルチモーダルモデルでの画像解析に失敗しました: {str(last_error)}"
            logger.error(error_msg)
            return f"画像の解析に失敗しました。{error_msg}"

    except Exception as e:
        logger.error(f"画像解析エラー: {str(e)}", exc_info=True)
        return f"画像の解析中にエラーが発生しました: {str(e)}"


@app.event("app_mention")
def handle_app_mentions(body, say):
    """メンションイベントのハンドラ"""
    try:
        event = body["event"]
        user_id = event["user"]
        text = event["text"]
        thread_ts = event.get(
            "thread_ts", event["ts"]
        )  # スレッドがあればそのIDを取得、なければ元メッセージのIDを使用
        channel_id = event.get("channel")

        logger.info(
            f"メンション受信: ユーザー {user_id}, チャンネル {channel_id}, スレッド {thread_ts}"
        )

        # ファイルが添付されているか確認
        files = event.get("files", [])
        if files:
            for file in files:
                mimetype = file.get("mimetype", "")
                logger.info(f"ファイル検出: {file.get('name')}, タイプ: {mimetype}")

                # ファイルタイプに応じて処理
                if mimetype.startswith("image/"):
                    handle_image_message(
                        file, text, user_id, channel_id, thread_ts, say
                    )
                    return
                elif mimetype == "text/csv" or file.get("name", "").lower().endswith(
                    ".csv"
                ):
                    handle_csv_message(file, text, user_id, channel_id, thread_ts, say)
                    return
                elif mimetype == "application/pdf" or file.get(
                    "name", ""
                ).lower().endswith(".pdf"):
                    handle_pdf_message(file, text, user_id, channel_id, thread_ts, say)
                    return
                else:
                    # その他のファイルタイプ
                    handle_generic_file(file, text, user_id, channel_id, thread_ts, say)
                    return

        # メンション部分（<@BOTID>）を削除
        cleaned_text = re.sub(r"<@[A-Z0-9]+>", "", text).strip()
        logger.debug(f"クリーニング後のテキスト: '{cleaned_text}'")

        # ユーザーのAIインスタンスを取得
        ai = get_ai_for_user(user_id)

        # 応答を生成して送信（スレッドで返信）
        logger.debug(f"メンションへの応答処理を開始します: ユーザー {user_id}")
        process_and_respond(ai, cleaned_text, say, thread_ts)

        logger.info(
            f"メンションに応答しました: ユーザー {user_id}, メッセージ: '{cleaned_text[:30]}...'"
        )

    except Exception as e:
        logger.error(f"メンション処理中にエラーが発生しました: {str(e)}", exc_info=True)
        say(f"エラーが発生しました: {str(e)}", thread_ts=thread_ts)


@app.event("message")
def handle_message_events(body, say):
    """メッセージイベントのハンドラ（DMの場合）"""
    try:
        event = body["event"]

        # ボットのメッセージは処理しない
        if event.get("bot_id"):
            return

        # DMの場合のみ処理
        if event.get("channel_type") == "im":
            user_id = event["user"]
            text = event["text"]
            thread_ts = event.get(
                "thread_ts", event["ts"]
            )  # スレッドがあればそのIDを取得
            channel_id = event.get("channel")

            logger.info(
                f"DM受信: ユーザー {user_id}, チャンネル {channel_id}, スレッド {thread_ts}"
            )
            logger.debug(f"メッセージ内容: '{text}'")

            # ファイルが添付されているか確認
            files = event.get("files", [])
            if files:
                for file in files:
                    mimetype = file.get("mimetype", "")
                    logger.info(f"ファイル検出: {file.get('name')}, タイプ: {mimetype}")

                    # ファイルタイプに応じて処理
                    if mimetype.startswith("image/"):
                        handle_image_message(
                            file, text, user_id, channel_id, thread_ts, say
                        )
                        return
                    elif mimetype == "text/csv" or file.get(
                        "name", ""
                    ).lower().endswith(".csv"):
                        handle_csv_message(
                            file, text, user_id, channel_id, thread_ts, say
                        )
                        return
                    elif mimetype == "application/pdf" or file.get(
                        "name", ""
                    ).lower().endswith(".pdf"):
                        handle_pdf_message(
                            file, text, user_id, channel_id, thread_ts, say
                        )
                        return
                    else:
                        # その他のファイルタイプ
                        handle_generic_file(
                            file, text, user_id, channel_id, thread_ts, say
                        )
                        return

            # 通常のテキストメッセージ処理
            # ユーザーのAIインスタンスを取得
            ai = get_ai_for_user(user_id)

            # 応答を生成して送信
            logger.debug(f"DMへの応答処理を開始します: ユーザー {user_id}")
            process_and_respond(ai, text, say, thread_ts)

            logger.info(
                f"DMに応答しました: ユーザー {user_id}, メッセージ: '{text[:30]}...'"
            )

    except Exception as e:
        logger.error(f"メッセージ処理中にエラーが発生しました: {str(e)}", exc_info=True)
        if event.get("channel"):
            say(f"エラーが発生しました: {str(e)}", thread_ts=thread_ts)


def process_and_respond(
    ai: PersonalAI, text: str, say, thread_ts: Optional[str] = None
):
    """
    メッセージを処理して応答を返す

    Args:
        ai: ユーザーのAIインスタンス
        text: メッセージテキスト
        say: 応答送信関数
        thread_ts: 返信するスレッドのタイムスタンプ
    """
    logger.debug(f"メッセージ処理開始: '{text}'")

    # 処理開始時にステータスを「返信中！」に変更
    set_bot_status(
        status_text="返信中！", status_emoji=":hourglass_flowing_sand:", presence="auto"
    )

    try:
        # 特別なコマンド処理
        if text.startswith("!help") or text == "!":
            logger.info("ヘルプコマンドを処理します")
            help_response = handle_help_command(ai)
            say(help_response, thread_ts=thread_ts)
            logger.debug(f"ヘルプコマンド応答送信完了: {len(help_response)} 文字")
            return

        # コマンド変換: !command → /command
        if text.startswith("!"):
            new_text = "/" + text[1:]
            logger.debug(f"コマンド変換: '{text}' → '{new_text}'")
            text = new_text

        # ターミナルコマンド実行パターンの検出
        terminal_patterns = [r"^\$\s+(.+)$", r"^実行\s*[:：]\s*(.+)$", r"^ターミナル\s*[:：]\s*(.+)$", r"^コマンド\s*[:：]\s*(.+)$"]
        terminal_command = None
        
        for pattern in terminal_patterns:
            match = re.match(pattern, text)
            if match:
                terminal_command = match.group(1)
                logger.info(f"ターミナルコマンドを検出: '{terminal_command}'")
                break
        
        # ターミナルコマンドが検出された場合は実行
        if terminal_command and MCP_AVAILABLE:
            # 処理中メッセージを送信
            say(f"コマンド `{terminal_command}` を実行中...", thread_ts=thread_ts)
            
            # 非同期関数でコマンドを実行
            async def run_detected_command():
                try:
                    # コマンド実行
                    result = await mcp_terminal_controller.execute_command(terminal_command)
                    
                    # 結果が長すぎる場合は切り詰める
                    if len(result) > 3000:
                        result = result[:3000] + "\n... (結果が長すぎるため切り詰められました)"
                    
                    # 結果を送信
                    say(f"コマンド `{terminal_command}` の実行結果:\n```\n{result}\n```", thread_ts=thread_ts)
                    
                    # 会話に保存
                    ai.save_conversation(f"[ターミナルコマンド] {terminal_command}", result, None)
                    
                except Exception as e:
                    error_msg = f"コマンド実行中にエラーが発生しました: {str(e)}"
                    logger.error(error_msg, exc_info=True)
                    say(error_msg, thread_ts=thread_ts)
            
            # 非同期関数を実行
            loop = asyncio.new_event_loop()
            asyncio.set_event_loop(loop)
            loop.run_until_complete(run_detected_command())
            loop.close()
            return

        # 記憶確認応答処理
        if ai.memory_candidate is not None:
            logger.info(f"記憶確認応答処理: ユーザー入力 '{text}'")
            if text.lower() in ["y", "yes", "はい", "うん", "そう"]:
                key = ai.memory_candidate["key"]
                value = ai.memory_candidate["value"]
                logger.info(f"記憶を保存: '{key}: {value}'")
                ai.save_profile_info(key, value)
                ai.memory_candidate = None
                say(f"「{key}: {value}」を記憶しました。", thread_ts=thread_ts)
                return
            elif text.lower() in ["n", "no", "いいえ", "違う", "ちがう"]:
                logger.info("記憶確認拒否")
                ai.memory_candidate = None
                say("わかりました。記憶しません。", thread_ts=thread_ts)
                return

        # リンクコマンド処理
        if text.startswith("/link"):
            logger.info(f"リンクコマンド検出: '{text}'")
            # ユーザーIDを取得
            user_id = ai.user_id.replace("slack_", "")

            # リンクコードを生成
            link_code = user_link_manager.generate_link_code(user_id)

            # コマンドラインツール用の使い方を表示
            cli_command = f"personal-ai link {link_code}"

            say(
                f"*CLIとSlackアカウントのリンク*\n\nCLIアプリケーションで次のコマンドを実行してリンクを完了してください:\n```\n{cli_command}\n```\n\nこのリンクコードは現在無期限で有効です（テスト中）。",
                thread_ts=thread_ts,
            )
            return

        # browser-useコマンド処理
        if text.startswith("/browser") or text.startswith("/browse"):
            logger.info(f"ブラウザ使用コマンド検出: '{text}'")
            # ユーザーIDを取得
            user_id = ai.user_id.replace("slack_", "")

            # タスク内容を取得
            task = text.split(maxsplit=1)[1] if len(text.split(maxsplit=1)) > 1 else ""
            if not task:
                say(
                    "タスクが指定されていません。`/browser <タスク内容>` の形式で指定してください。",
                    thread_ts=thread_ts,
                )
                return

            # チャンネルIDを取得
            channel_id = None
            if thread_ts:
                try:
                    client = WebClient(token=SLACK_BOT_TOKEN)
                    response = client.conversations_replies(
                        channel="C067RCQEZQX",  # デフォルトチャンネル
                        ts=thread_ts,
                    )
                    if response and response.get("ok"):
                        channel_id = response.get("channel")
                except Exception as e:
                    logger.warning(f"チャンネルID取得中にエラー: {str(e)}")
                    channel_id = "C067RCQEZQX"  # デフォルトチャンネル

            # ブラウザタスクを実行
            handle_browser_use(ai, task, user_id, channel_id, thread_ts, say)
            return

        # 画像表示リクエストの処理（優先的に処理）
        if text.startswith("/image") or is_image_request(text):
            logger.info(f"画像リクエスト検出: '{text}'")
            # スレッドのチャンネルIDを取得
            channel_id = None
            if thread_ts:
                try:
                    client = WebClient(token=SLACK_BOT_TOKEN)
                    # スレッド情報からチャンネルIDを取得する試み
                    for channel_type in ["conversations.replies", "channels.replies"]:
                        try:
                            if channel_type == "conversations.replies":
                                response = client.conversations_replies(
                                    channel="C067RCQEZQX",  # デフォルトチャンネル
                                    ts=thread_ts,
                                )
                            else:
                                response = client.channels_replies(
                                    channel="C067RCQEZQX",  # デフォルトチャンネル
                                    thread_ts=thread_ts,
                                )
                            if response and response.get("ok"):
                                channel_id = response.get("channel")
                                logger.info(
                                    f"スレッド情報からチャンネルID '{channel_id}' を取得しました"
                                )
                                break
                        except Exception as e:
                            logger.debug(
                                f"{channel_type}でのチャンネルID取得に失敗: {str(e)}"
                            )
                            continue
                except Exception as e:
                    logger.warning(f"チャンネルID取得中にエラー: {str(e)}")
                    channel_id = "C067RCQEZQX"  # デフォルトチャンネル

            process_image_request(ai, text, say, thread_ts, channel_id)
            return

        # コマンド処理
        if text.startswith("/"):
            logger.info(f"コマンド検出: '{text}'")
            cmd_parts = text.split()
            cmd = cmd_parts[0][1:]  # 先頭の/を除去
            cmd_args = cmd_parts[1:] if len(cmd_parts) > 1 else []

            if cmd in ai.command_handlers:
                logger.info(f"コマンド実行: {cmd}, 引数: {cmd_args}")
                try:
                    result = ai.command_handlers[f"/{cmd}"](cmd_args)
                    logger.debug(f"コマンド実行結果: {result[:100]}...")
                    say(format_command_result(result), thread_ts=thread_ts)
                    return
                except Exception as e:
                    logger.error(f"コマンド実行エラー: {str(e)}", exc_info=True)
                    say(
                        f"コマンド実行中にエラーが発生しました: {str(e)}",
                        thread_ts=thread_ts,
                    )
                    return

        # 通常の会話処理
        logger.info("AI応答を生成します")
        start_time = datetime.now()

        # LLMを使用して入力の意図を分析
        intent_analysis = ai.analyze_input_intent(text)
        logger.info(f"入力意図分析結果: {intent_analysis}")

        # 検索タスクの検出
        is_search_request = intent_analysis["intent_type"] == "search"

        if is_search_request:
            # 検索タスクとして処理
            logger.info(f"検索リクエスト検出: '{text}'")

            # browser-useが利用可能か実行時に確認
            browser_use_available = False
            try:
                from browser_use import Agent as BrowserAgent
                from langchain_ollama import ChatOllama
                from playwright.sync_api import sync_playwright
                from langchain.chains import LLMChain
                from langchain.prompts import PromptTemplate

                browser_use_available = True
                logger.info(
                    "検索リクエスト: browser-useライブラリが正常にインポートされました"
                )
            except ImportError as e:
                browser_use_available = False
                error_msg = (
                    f"検索リクエスト: browser-useのインポートに失敗しました: {str(e)}"
                )
                logger.error(error_msg)

            if not browser_use_available:
                # browser-useが利用できない場合は、通常のAI応答で検索内容について答える
                logger.info(
                    "browser-useが利用できないため、通常のAI応答で検索内容について回答します"
                )
                search_prompt = f"ユーザーから「{text}」という検索リクエストを受け取りました。browser-useが利用できないため、あなたの知識に基づいて回答してください。"
                response = ai.generate_response(search_prompt)
                say(response, thread_ts=thread_ts)
                return

            # チャンネルIDを取得
            channel_id = None
            if thread_ts:
                try:
                    client = WebClient(token=SLACK_BOT_TOKEN)
                    response = client.conversations_replies(
                        channel="C067RCQEZQX",
                        ts=thread_ts,
                    )
                    if response and response.get("ok"):
                        channel_id = response.get("channel")
                except Exception:
                    channel_id = "C067RCQEZQX"

            # ユーザーIDを取得
            user_id = ai.user_id.replace("slack_", "")

            # 検索タスクを実行
            search_task = f"以下の内容について検索してください: {text}"
            handle_browser_use(ai, search_task, user_id, channel_id, thread_ts, say)
            return

        response = ai.generate_response(text)
        end_time = datetime.now()
        duration = (end_time - start_time).total_seconds()
        logger.info(
            f"AI応答生成完了: 処理時間 {duration:.2f}秒, 応答長 {len(response)} 文字"
        )

        # 記憶確認が含まれる場合は特別な形式で表示
        if ai.memory_candidate:
            logger.debug("記憶候補を検出、確認メッセージを追加")
            main_response, confirmation = response.rsplit("\n\n", 1)
            say(f"{main_response}\n\n*確認*: {confirmation}", thread_ts=thread_ts)
        else:
            # 通常の応答表示
            logger.debug(f"通常応答を送信: {response[:100]}...")
            say(response, thread_ts=thread_ts)
    except Exception as e:
        logger.error(f"応答生成中にエラーが発生しました: {str(e)}", exc_info=True)
        say(f"応答生成中にエラーが発生しました: {str(e)}", thread_ts=thread_ts)
    finally:
        # 処理完了時にステータスを「起動中！」に戻す
        set_bot_status(
            status_text="起動中！", status_emoji=":robot_face:", presence="auto"
        )


def handle_image_message(
    file: Dict[str, Any], text: str, user_id: str, channel_id: str, thread_ts: str, say
):
    """
    画像メッセージを処理

    Args:
        file: ファイルオブジェクト
        text: メッセージテキスト
        user_id: ユーザーID
        channel_id: チャンネルID
        thread_ts: スレッドID
        say: 応答関数
    """
    image_path = None
    download_attempts = 0
    max_download_attempts = 3

    try:
        file_info = {
            "id": file.get("id", "不明"),
            "name": file.get("name", "不明"),
            "mimetype": file.get("mimetype", "不明"),
            "filetype": file.get("filetype", "不明"),
            "size": file.get("size", "不明"),
            "url_private": file.get("url_private", "不明"),
            "dimensions": f"{file.get('original_w', '?')}x{file.get('original_h', '?')}",
        }

        logger.info(
            f"画像メッセージを検出: ユーザー {user_id}, ファイル情報: {json.dumps(file_info)}"
        )

        # 処理中メッセージを送信
        say("画像を解析中です...", thread_ts=thread_ts)

        # ダウンロードURLの選択（優先順位: url_private_download > url_private > thumb_1024 > thumb_720 > thumb_480）
        download_urls = []

        if file.get("url_private_download"):
            download_urls.append(
                {"url": file["url_private_download"], "type": "オリジナルダウンロード"}
            )

        if file.get("url_private"):
            download_urls.append({"url": file["url_private"], "type": "オリジナル"})

        # サムネイルもバックアップとして追加（大きいものから優先）
        for size in ["1024", "800", "720", "480", "360"]:
            thumb_key = f"thumb_{size}"
            if file.get(thumb_key):
                download_urls.append(
                    {"url": file[thumb_key], "type": f"サムネイル {size}"}
                )

        # URLがない場合はエラー
        if not download_urls:
            say("画像URLが見つかりません。", thread_ts=thread_ts)
            return

        # 複数のURLを順番に試行
        for download_info in download_urls:
            download_url = download_info["url"]
            url_type = download_info["type"]
            logger.info(
                f"{url_type}のURLを使用してダウンロードを試みます: {download_url}"
            )

            original_filename = file.get("name", "image")
            image_path, mime_type = download_file(
                download_url, SLACK_BOT_TOKEN, original_filename
            )

            if image_path and os.path.exists(image_path):
                logger.info(
                    f"{url_type}のURLから画像の取得に成功しました: {image_path}"
                )
                break
            else:
                logger.warning(f"{url_type}のURLからの画像取得に失敗しました")

        # すべてのURLで失敗した場合
        if not image_path or not os.path.exists(image_path):
            say(
                "すべてのURLから画像のダウンロードに失敗しました。", thread_ts=thread_ts
            )
            return

        # 画像情報をさらに詳しくログに出力
        file_size = (
            os.path.getsize(image_path) if os.path.exists(image_path) else "不明"
        )
        logger.info(
            f"画像ファイル情報: パス={image_path}, サイズ={file_size}バイト, MIME={mime_type}"
        )

        # 画像のデータを検証（より強固なエラーハンドリング）
        img_verified = False
        img_info = "未検証"
        img_format = "不明"

        try:
            with Image.open(image_path) as img:
                # 実際に画像データにアクセスして検証
                img.load()
                img_format = img.format or "不明"
                img_size = img.size
                img_mode = img.mode
                img_info = (
                    f"フォーマット={img_format}, サイズ={img_size}, モード={img_mode}"
                )
                logger.info(f"PIL画像検証成功: {img_info}")
                img_verified = True

                # サムネイルを生成して保存
                try:
                    thumbnail_path = f"{image_path}_thumbnail.jpg"
                    img.thumbnail((300, 300))

                    # RGBA画像をRGBに変換してからJPEGとして保存
                    if img.mode == "RGBA":
                        # アルファチャネルの処理方法1: 白背景で合成
                        rgb_img = Image.new("RGB", img.size, (255, 255, 255))
                        rgb_img.paste(img, (0, 0), img.split()[3])
                        rgb_img.save(thumbnail_path, "JPEG")
                        logger.info(f"RGBA→RGB変換してサムネイル生成: {thumbnail_path}")
                    else:
                        img.save(thumbnail_path, "JPEG")
                        logger.info(f"サムネイル生成: {thumbnail_path}")
                except Exception as thumb_err:
                    logger.warning(f"サムネイル生成エラー: {str(thumb_err)}")
                    # サムネイル生成失敗はダウンロード成功の妨げにはならない

        except Exception as img_err:
            logger.error(f"画像検証中にエラー: {str(img_err)}", exc_info=True)
            say(
                f"画像が正しく開けませんでした。フォーマットが不正か破損している可能性があります。エラー: {str(img_err)}",
                thread_ts=thread_ts,
            )
            return

        # テキストを確認して、質問があれば使用
        cleaned_text = text
        if text:
            # メンション部分を削除
            for mention_match in re.finditer(r"<@([A-Z0-9]+)>", text):
                user_mention = mention_match.group(0)
                cleaned_text = cleaned_text.replace(user_mention, "").strip()

        # プロンプトが空の場合はデフォルトのプロンプトを使用
        prompt = (
            cleaned_text
            if cleaned_text and cleaned_text.strip() != ""
            else "この画像について詳しく説明してください。何が写っていますか？"
        )

        # 画像フォーマット情報をプロンプトに追加
        prompt_with_info = f"{prompt} (画像情報: {img_info})"

        # 画像を解析
        logger.info(f"画像解析を開始: プロンプト: '{prompt}'")
        analysis_result = analyze_image_with_ollama(image_path, prompt_with_info)

        # 応答を送信
        say(f"画像解析結果:\n\n{analysis_result}", thread_ts=thread_ts)
        logger.info("画像解析結果を送信しました")

        # ユーザーのAIインスタンスを取得
        ai = get_ai_for_user(user_id)

        # 画像の保存処理
        # 画像保存の指示があるか確認
        should_save_image = False
        save_keywords = [
            "保存",
            "セーブ",
            "記録",
            "覚えて",
            "覚える",
            "保持",
            "キープ",
            "save",
            "store",
            "keep",
            "remember",
        ]

        for keyword in save_keywords:
            if keyword in prompt.lower():
                should_save_image = True
                break

        if should_save_image:
            # 画像のタイトルを生成（ファイル名またはデフォルト）
            image_title = file.get("name", "Slack画像") or "Slack画像"

            # 説明文は解析結果の最初の数行を使用
            image_desc = analysis_result.split("\n")[0] if analysis_result else ""
            if len(image_desc) > 100:
                image_desc = image_desc[:100] + "..."

            # 画像を保存
            try:
                image_id = ai.memory.save_image(image_path, image_title, image_desc)
                if image_id:
                    say(
                        f"画像を保存しました (ID: {image_id})。\n`/image show {image_id}` で表示できます。",
                        thread_ts=thread_ts,
                    )
                    logger.info(f"画像を保存しました: {image_id}")
                else:
                    say("画像の保存に失敗しました。", thread_ts=thread_ts)
            except Exception as e:
                logger.error(f"画像保存中にエラー: {str(e)}", exc_info=True)
                say(f"画像保存中にエラーが発生しました: {str(e)}", thread_ts=thread_ts)

        # プロファイル情報抽出のためのAI処理を実行
        try:
            # 画像解析結果からプロファイル情報を抽出するプロンプトを作成
            extraction_prompt = f"""
以下の画像解析結果から、ユーザープロファイルに保存すべき重要な情報があれば抽出してください。
キーと値のペアの形式で返してください。情報がない場合は「なし」と返してください。

解析結果:
{analysis_result}
"""

            logger.info("画像解析結果からプロファイル情報を抽出中")
            extract_response = ollama.chat(
                model=OLLAMA_MODEL,
                messages=[
                    {
                        "role": "system",
                        "content": "あなたはユーザープロファイル情報を抽出するアシスタントです。与えられたテキストから重要な情報をキーと値のペアで抽出してください。",
                    },
                    {"role": "user", "content": extraction_prompt},
                ],
            )

            extracted_info = extract_response["message"]["content"].strip()
            logger.debug(f"抽出されたプロファイル情報: {extracted_info}")

            # 「なし」以外の応答があれば処理
            if extracted_info.lower() not in ["なし", "none", "no information"]:
                # キーと値のペアを検出
                # 例: "好きな食べ物: ラーメン" や "趣味: 旅行"など
                info_matches = re.findall(r"([^:]+):\s*(.+?)(?:\n|$)", extracted_info)

                if info_matches:
                    # 最初に見つかった情報を使用
                    key, value = info_matches[0]
                    key = key.strip()
                    value = value.strip()

                    if key and value:
                        logger.info(f"画像から抽出したプロファイル情報: {key}: {value}")

                        # 記憶すべき情報があれば候補として保存（次の入力でy/nを確認する）
                        ai.memory_candidate = {"key": key, "value": value}
                        say(
                            f"画像から「{key}: {value}」という情報を見つけました。プロファイルに保存しますか？ (はい/いいえ)",
                            thread_ts=thread_ts,
                        )
        except Exception as extract_err:
            logger.error(
                f"プロファイル情報抽出中にエラー: {str(extract_err)}", exc_info=True
            )
            # エラーが発生しても会話履歴の保存は続行

        # 対話履歴に保存
        ai.save_conversation(
            f"[画像を共有] {prompt}", analysis_result, None  # 新しい会話IDを生成
        )

    except Exception as e:
        error_msg = f"画像処理中にエラーが発生しました: {str(e)}"
        logger.error(error_msg, exc_info=True)
        say(error_msg, thread_ts=thread_ts)

        # エラーが発生した場合でも会話を保存
        try:
            ai = get_ai_for_user(user_id)
            ai.save_conversation(
                f"[画像を共有] {text if text else '画像を共有'}",
                f"エラー: {str(e)}",
                None,
            )
        except Exception as save_error:
            logger.error(
                f"会話の保存中にエラーが発生しました: {str(save_error)}", exc_info=True
            )
    finally:
        # 一時ファイルのクリーンアップ (デバッグモードの場合は保持)
        try:
            if not DEBUG_MODE and image_path and os.path.exists(image_path):
                os.unlink(image_path)
                logger.debug(f"一時ファイルを削除しました: {image_path}")
        except Exception as e:
            logger.warning(f"一時ファイルの削除に失敗しました: {str(e)}")


def handle_csv_message(
    file: Dict[str, Any], text: str, user_id: str, channel_id: str, thread_ts: str, say
):
    """
    CSVファイルメッセージを処理

    Args:
        file: ファイルオブジェクト
        text: メッセージテキスト
        user_id: ユーザーID
        channel_id: チャンネルID
        thread_ts: スレッドID
        say: 応答関数
    """
    file_path = None

    try:
        file_info = {
            "id": file.get("id", "不明"),
            "name": file.get("name", "不明"),
            "mimetype": file.get("mimetype", "不明"),
            "filetype": file.get("filetype", "不明"),
            "size": file.get("size", "不明"),
            "url_private": file.get("url_private", "不明"),
        }

        logger.info(
            f"CSVファイルメッセージを検出: ユーザー {user_id}, ファイル情報: {json.dumps(file_info)}"
        )

        # 処理中メッセージを送信
        say("CSVファイルを解析中です...", thread_ts=thread_ts)

        # ダウンロードURLの選択
        download_urls = []

        if file.get("url_private_download"):
            download_urls.append(
                {"url": file["url_private_download"], "type": "オリジナルダウンロード"}
            )

        if file.get("url_private"):
            download_urls.append({"url": file["url_private"], "type": "オリジナル"})

        # URLがない場合はエラー
        if not download_urls:
            say("CSVファイルのURLが見つかりません。", thread_ts=thread_ts)
            return

        # 複数のURLを順番に試行
        for download_info in download_urls:
            download_url = download_info["url"]
            url_type = download_info["type"]
            logger.info(
                f"{url_type}のURLを使用してダウンロードを試みます: {download_url}"
            )

            original_filename = file.get("name", "file.csv")
            file_path, mime_type = download_file(
                download_url, SLACK_BOT_TOKEN, original_filename
            )

            if file_path and os.path.exists(file_path):
                logger.info(
                    f"{url_type}のURLからCSVファイルの取得に成功しました: {file_path}"
                )
                break
            else:
                logger.warning(f"{url_type}のURLからのCSVファイル取得に失敗しました")

        # すべてのURLで失敗した場合
        if not file_path or not os.path.exists(file_path):
            say("CSVファイルのダウンロードに失敗しました。", thread_ts=thread_ts)
            return

        # テキストを確認して、質問があれば使用
        cleaned_text = text
        if text:
            # メンション部分を削除
            for mention_match in re.finditer(r"<@([A-Z0-9]+)>", text):
                user_mention = mention_match.group(0)
                cleaned_text = cleaned_text.replace(user_mention, "").strip()

        # プロンプトが空の場合はデフォルトのプロンプトを使用
        prompt = (
            cleaned_text
            if cleaned_text and cleaned_text.strip() != ""
            else "このCSVファイルの内容を要約し、主要なデータポイントを説明してください。"
        )

        # CSVファイルを解析
        logger.info(f"CSVファイル解析を開始: プロンプト: '{prompt}'")
        analysis_result = analyze_csv_file(file_path, prompt)

        # 応答を送信
        say(f"CSVファイル解析結果:\n\n{analysis_result}", thread_ts=thread_ts)
        logger.info("CSVファイル解析結果を送信しました")

        # ユーザーのAIインスタンスを取得して対話履歴に保存
        ai = get_ai_for_user(user_id)
        ai.save_conversation(
            f"[CSVファイル共有] {prompt}", analysis_result, None  # 新しい会話IDを生成
        )

    except Exception as e:
        error_msg = f"CSVファイル処理中にエラーが発生しました: {str(e)}"
        logger.error(error_msg, exc_info=True)
        say(error_msg, thread_ts=thread_ts)

        # エラーが発生した場合でも会話を保存
        try:
            ai = get_ai_for_user(user_id)
            ai.save_conversation(
                f"[CSVファイル共有] {text if text else 'CSVファイルを共有'}",
                f"エラー: {str(e)}",
                None,
            )
        except Exception as save_error:
            logger.error(
                f"会話の保存中にエラーが発生しました: {str(save_error)}", exc_info=True
            )
    finally:
        # 一時ファイルのクリーンアップ (デバッグモードの場合は保持)
        try:
            if not DEBUG_MODE and file_path and os.path.exists(file_path):
                os.unlink(file_path)
                logger.debug(f"一時ファイルを削除しました: {file_path}")
        except Exception as e:
            logger.warning(f"一時ファイルの削除に失敗しました: {str(e)}")


def handle_pdf_message(
    file: Dict[str, Any], text: str, user_id: str, channel_id: str, thread_ts: str, say
):
    """
    PDFファイルメッセージを処理

    Args:
        file: ファイルオブジェクト
        text: メッセージテキスト
        user_id: ユーザーID
        channel_id: チャンネルID
        thread_ts: スレッドID
        say: 応答関数
    """
    file_path = None

    try:
        file_info = {
            "id": file.get("id", "不明"),
            "name": file.get("name", "不明"),
            "mimetype": file.get("mimetype", "不明"),
            "filetype": file.get("filetype", "不明"),
            "size": file.get("size", "不明"),
            "url_private": file.get("url_private", "不明"),
        }

        logger.info(
            f"PDFファイルメッセージを検出: ユーザー {user_id}, ファイル情報: {json.dumps(file_info)}"
        )

        # 処理中メッセージを送信
        say("PDFファイルを解析中です...", thread_ts=thread_ts)

        # ダウンロードURLの選択
        download_urls = []

        if file.get("url_private_download"):
            download_urls.append(
                {"url": file["url_private_download"], "type": "オリジナルダウンロード"}
            )

        if file.get("url_private"):
            download_urls.append({"url": file["url_private"], "type": "オリジナル"})

        # URLがない場合はエラー
        if not download_urls:
            say("PDFファイルのURLが見つかりません。", thread_ts=thread_ts)
            return

        # 複数のURLを順番に試行
        for download_info in download_urls:
            download_url = download_info["url"]
            url_type = download_info["type"]
            logger.info(
                f"{url_type}のURLを使用してダウンロードを試みます: {download_url}"
            )

            original_filename = file.get("name", "file.pdf")
            file_path, mime_type = download_file(
                download_url, SLACK_BOT_TOKEN, original_filename
            )

            if file_path and os.path.exists(file_path):
                logger.info(
                    f"{url_type}のURLからPDFファイルの取得に成功しました: {file_path}"
                )
                break
            else:
                logger.warning(f"{url_type}のURLからのPDFファイル取得に失敗しました")

        # すべてのURLで失敗した場合
        if not file_path or not os.path.exists(file_path):
            say("PDFファイルのダウンロードに失敗しました。", thread_ts=thread_ts)
            return

        # テキストを確認して、質問があれば使用
        cleaned_text = text
        if text:
            # メンション部分を削除
            for mention_match in re.finditer(r"<@([A-Z0-9]+)>", text):
                user_mention = mention_match.group(0)
                cleaned_text = cleaned_text.replace(user_mention, "").strip()

        # プロンプトが空の場合はデフォルトのプロンプトを使用
        prompt = (
            cleaned_text
            if cleaned_text and cleaned_text.strip() != ""
            else "このPDFの内容を要約してください。主要なポイントを教えてください。"
        )

        # PDFファイルを解析
        logger.info(f"PDFファイル解析を開始: プロンプト: '{prompt}'")
        analysis_result = analyze_pdf_file(file_path, prompt)

        # 応答を送信
        say(f"PDFファイル解析結果:\n\n{analysis_result}", thread_ts=thread_ts)
        logger.info("PDFファイル解析結果を送信しました")

        # ユーザーのAIインスタンスを取得して対話履歴に保存
        ai = get_ai_for_user(user_id)
        ai.save_conversation(
            f"[PDFファイル共有] {prompt}", analysis_result, None  # 新しい会話IDを生成
        )

    except Exception as e:
        error_msg = f"PDFファイル処理中にエラーが発生しました: {str(e)}"
        logger.error(error_msg, exc_info=True)
        say(error_msg, thread_ts=thread_ts)

        # エラーが発生した場合でも会話を保存
        try:
            ai = get_ai_for_user(user_id)
            ai.save_conversation(
                f"[PDFファイル共有] {text if text else 'PDFファイルを共有'}",
                f"エラー: {str(e)}",
                None,
            )
        except Exception as save_error:
            logger.error(
                f"会話の保存中にエラーが発生しました: {str(save_error)}", exc_info=True
            )
    finally:
        # 一時ファイルのクリーンアップ (デバッグモードの場合は保持)
        try:
            if not DEBUG_MODE and file_path and os.path.exists(file_path):
                os.unlink(file_path)
                logger.debug(f"一時ファイルを削除しました: {file_path}")
        except Exception as e:
            logger.warning(f"一時ファイルの削除に失敗しました: {str(e)}")


def handle_generic_file(
    file: Dict[str, Any], text: str, user_id: str, channel_id: str, thread_ts: str, say
):
    """
    その他の一般的なファイルを処理

    Args:
        file: ファイルオブジェクト
        text: メッセージテキスト
        user_id: ユーザーID
        channel_id: チャンネルID
        thread_ts: スレッドID
        say: 応答関数
    """
    try:
        file_info = {
            "id": file.get("id", "不明"),
            "name": file.get("name", "不明"),
            "mimetype": file.get("mimetype", "不明"),
            "filetype": file.get("filetype", "不明"),
            "size": file.get("size", "不明"),
        }

        logger.info(
            f"ファイルメッセージを検出: ユーザー {user_id}, ファイル情報: {json.dumps(file_info)}"
        )

        # ユーザーのAIインスタンスを取得
        ai = get_ai_for_user(user_id)

        # テキストを確認して、質問があれば使用
        cleaned_text = text
        if text:
            # メンション部分を削除
            for mention_match in re.finditer(r"<@([A-Z0-9]+)>", text):
                user_mention = mention_match.group(0)
                cleaned_text = cleaned_text.replace(user_mention, "").strip()

        # ファイル情報を含めた応答を生成
        file_type = file.get("mimetype", "不明")
        file_name = file.get("name", "不明")

        if not cleaned_text or cleaned_text.strip() == "":
            # メッセージがない場合、ファイル情報だけに基づいて応答
            response = ai.generate_response(
                f"ユーザーが '{file_name}' という名前の {file_type} ファイルを共有しました。"
                f"このファイルについて何ができるか説明してください。"
            )
        else:
            # メッセージがある場合、それも含めて応答
            response = ai.generate_response(
                f"ユーザーが '{file_name}' という名前の {file_type} ファイルを共有しました。"
                f"また、以下のメッセージも送信しています: '{cleaned_text}'。"
                f"このファイルとメッセージに基づいて応答してください。"
            )

        # 応答を送信
        say(response, thread_ts=thread_ts)
        logger.info("ファイル情報に基づく応答を送信しました")

    except Exception as e:
        error_msg = f"ファイル処理中にエラーが発生しました: {str(e)}"
        logger.error(error_msg, exc_info=True)
        say(error_msg, thread_ts=thread_ts)


def is_image_request(text: str) -> bool:
    """
    テキストが画像要求かどうかを判定する

    Args:
        text: メッセージテキスト

    Returns:
        画像要求かどうか
    """
    image_request_patterns = [
        r"画像.*表示",
        r"画像.*見せて",
        r"写真.*表示",
        r"写真.*見せて",
        r"イメージ.*表示",
        r"イメージ.*見せて",
        r"画像.*送って",
        r"写真.*送って",
        r"イメージ.*送って",
        r"画像.*送信",
        r"写真.*送信",
        r"イメージ.*送信",
        r".*見せて",  # 「〜を見せて」のパターンを追加
        r".*表示して",  # 「〜を表示して」のパターンを追加
        r"img_\d+",  # 画像IDのパターンを追加
    ]

    # 日本語と英語の両方に対応
    for pattern in image_request_patterns:
        if re.search(pattern, text, re.IGNORECASE):
            return True

    return False


def process_image_request(
    ai: PersonalAI,
    text: str,
    say,
    thread_ts: Optional[str] = None,
    channel_id: Optional[str] = None,
):
    """
    画像リクエストを処理

    Args:
        ai: ユーザーのAIインスタンス
        text: メッセージテキスト
        say: 応答送信関数
        thread_ts: 返信するスレッドのタイムスタンプ
        channel_id: Slackチャンネル ID
    """
    logger.debug(f"画像リクエスト処理開始: '{text}'")

    # チャンネルIDが指定されていない場合のデフォルト値設定
    if not channel_id:
        channel_id = "C067RCQEZQX"  # デフォルトチャンネルID
        logger.debug(
            f"チャンネルIDが指定されていないため、デフォルト値を使用: {channel_id}"
        )

    # /imageコマンドの処理
    if text.startswith("/image"):
        parts = text.split()
        if len(parts) < 2:
            say(
                "使用法: /image <サブコマンド>\nサブコマンド: list, show, search",
                thread_ts=thread_ts,
            )
            return

        subcommand = parts[1].lower()

        if subcommand == "list":
            # 画像一覧の表示
            metadata = ai.get_image_metadata()
            if metadata and metadata != "# 保存された画像一覧\n\n":
                say(f"保存された画像一覧:\n\n{metadata}", thread_ts=thread_ts)
            else:
                say("保存された画像はありません。", thread_ts=thread_ts)
            return

        elif subcommand == "show":
            # 特定の画像の表示
            if len(parts) < 3:
                say("使用法: /image show <画像ID>", thread_ts=thread_ts)
                return

            image_id = parts[2]
            image_path = ai.get_image_path(image_id)

            if not image_path or not os.path.exists(image_path):
                say(f"画像 '{image_id}' が見つかりません。", thread_ts=thread_ts)
                return

            # 画像メタデータを取得
            metadata = ai.get_image_metadata(image_id)

            # 画像をSlackにアップロード
            try:
                # ファイルアップロードAPIを使用
                from slack_sdk.web import WebClient

                client = WebClient(token=SLACK_BOT_TOKEN)

                # 応答を送信
                say(
                    f"画像 '{image_id}' を表示します:\n\n{metadata}",
                    thread_ts=thread_ts,
                )

                # ファイルをアップロード
                logger.info(
                    f"画像アップロード開始: パス={image_path}, チャンネル={channel_id}, スレッド={thread_ts}"
                )
                response = client.files_upload_v2(
                    file=image_path,
                    title=f"画像 {image_id}",
                    initial_comment=f"要求された画像 (ID: {image_id})",
                    thread_ts=thread_ts,
                    channel=channel_id,
                )

                logger.info(f"画像アップロード成功: {response}")
            except Exception as e:
                logger.error(f"画像アップロード中にエラー: {str(e)}", exc_info=True)
                say(f"画像のアップロードに失敗しました: {str(e)}", thread_ts=thread_ts)

            return

        elif subcommand == "search":
            # 画像検索
            if len(parts) < 3:
                say("使用法: /image search <検索キーワード>", thread_ts=thread_ts)
                return

            query = " ".join(parts[2:])
            results = ai.search_images(query)

            say(f"検索結果:\n\n{results}", thread_ts=thread_ts)
            return

        else:
            say(
                f"不明なサブコマンド: {subcommand}\n使用法: /image <サブコマンド>\nサブコマンド: list, show, search",
                thread_ts=thread_ts,
            )
            return

    # 自然言語での画像要求の処理
    else:
        # テキストから画像IDや検索キーワードを抽出
        image_id_match = re.search(r"(img_\d+)", text)

        if image_id_match:
            # 特定の画像IDが指定されている場合
            image_id = image_id_match.group(1)
            logger.info(f"テキストから画像ID '{image_id}' を検出しました")
            return process_image_request(
                ai, f"/image show {image_id}", say, thread_ts, channel_id
            )

        # 保存された画像のメタデータを取得
        metadata = ai.get_image_metadata()
        if not metadata or metadata == "# 保存された画像一覧\n\n":
            say("保存された画像はありません。", thread_ts=thread_ts)
            return

        # テキスト中のキーワードに基づいて画像を検索
        # 「世界平和なあやかさん」などの具体的な名前や説明に基づく検索
        search_keywords = (
            text.replace("見せて", "").replace("表示", "").replace("送って", "").strip()
        )

        if search_keywords:
            logger.info(f"画像検索キーワード: '{search_keywords}'")
            results = ai.search_images(search_keywords)

            if results and "検索結果はありません" not in results:
                # 検索結果から最初の画像IDを抽出
                image_id_match = re.search(r"(img_\d+)", results)
                if image_id_match:
                    image_id = image_id_match.group(1)
                    logger.info(f"検索結果から画像ID '{image_id}' を使用します")
                    return process_image_request(
                        ai, f"/image show {image_id}", say, thread_ts, channel_id
                    )
                else:
                    say(f"検索結果:\n\n{results}", thread_ts=thread_ts)
                    return
            else:
                # 最新の画像を表示
                image_id_match = re.search(r"(img_\d+)", metadata)
                if image_id_match:
                    image_id = image_id_match.group(1)
                    logger.info(
                        f"キーワード検索で結果がないため、最新の画像ID '{image_id}' を使用します"
                    )
                    return process_image_request(
                        ai, f"/image show {image_id}", say, thread_ts, channel_id
                    )

        # キーワードが特定できない場合は最新の画像を表示
        image_id_match = re.search(r"(img_\d+)", metadata)
        if image_id_match:
            image_id = image_id_match.group(1)
            logger.info(f"キーワードがないため、最新の画像ID '{image_id}' を使用します")
            return process_image_request(
                ai, f"/image show {image_id}", say, thread_ts, channel_id
            )
        else:
            # 画像IDが見つからない場合は一覧を表示
            logger.info("画像IDが見つからないため、画像一覧を表示します")
            return process_image_request(ai, "/image list", say, thread_ts, channel_id)


def handle_browser_use(
    ai: PersonalAI, task: str, user_id: str, channel_id: str, thread_ts: str, say
):
    """
    browser-useを使用してブラウザタスクを実行する

    Args:
        ai: ユーザーのAIインスタンス
        task: 実行するタスク内容
        user_id: ユーザーID
        channel_id: チャンネルID
        thread_ts: スレッドID
        say: 応答関数
    """
    # 実行時に必要なライブラリをインポートできるか確認
    browser_use_available = False
    try:
        from browser_use import Agent as BrowserAgent
        from langchain_ollama import ChatOllama
        from playwright.sync_api import sync_playwright
        from langchain.chains import LLMChain
        from langchain.prompts import PromptTemplate

        browser_use_available = True
        logger.info("browser-useライブラリが正常にインポートされました")

        # Playwrightが正しくインストールされているか確認
        try:
            from playwright.sync_api import sync_playwright

            logger.info("Playwrightがインストールされています")

            # ブラウザがインストールされているか確認
            with sync_playwright() as playwright:
                try:
                    browser = playwright.chromium.launch()
                    browser.close()
                    logger.info("Playwrightのブラウザが正常に起動しました")
                except Exception as browser_error:
                    logger.warning(
                        f"Playwrightのブラウザ起動に失敗: {str(browser_error)}"
                    )
                    say(
                        "Playwrightのブラウザがインストールされていないようです。以下のコマンドを実行してください: `rye run playwright install`",
                        thread_ts=thread_ts,
                    )
                    return

        except ImportError as pw_error:
            logger.error(f"Playwrightのインポートに失敗: {str(pw_error)}")
            say(
                "Playwrightがインストールされていないようです。以下のコマンドを実行してください: `rye run playwright install`",
                thread_ts=thread_ts,
            )
            return

    except ImportError as e:
        error_msg = f"browser-useのインポートに失敗しました: {str(e)}"
        logger.error(error_msg)
        say(
            'browser-useが利用できません。インストールするには: `rye add "browser-use==0.1.40" "langchain-openai==0.3.1" "langchain-ollama==0.2.2"`、そしてPlaywrightのセットアップ: `rye run playwright install`',
            thread_ts=thread_ts,
        )
        return

    if not browser_use_available:
        say(
            'browser-useが利用できません。インストールするには: `rye add "browser-use==0.1.40" "langchain-openai==0.3.1" "langchain-ollama==0.2.2"`、そしてPlaywrightのセットアップ: `rye run playwright install`',
            thread_ts=thread_ts,
        )
        return

    try:
        # 実行中メッセージを送信
        say(
            f"🔍 タスク分析を開始します: `{task}`",
            thread_ts=thread_ts,
        )

        # グローバル変数を更新して今後の確認をスキップ
        global BROWSER_USE_AVAILABLE
        BROWSER_USE_AVAILABLE = True

        # 思考プロセスと結果を保存する変数
        thought_process = []

        # 実行状況の更新を送信する関数
        async def status_updater(message, is_thinking=False):
            prefix = "💭 思考中: " if is_thinking else "🔄 進捗状況: "
            formatted_message = f"{prefix}{message}"
            say(formatted_message, thread_ts=thread_ts)
            # 思考プロセスを記録
            if is_thinking:
                thought_process.append(message)

        # タスクをToDoリストに変換する関数
        async def create_todo_list(task_description):
            # ToDoリストを生成するためのプロンプト
            todo_prompt = f"""
以下のタスクを具体的なToDoリストに分解してください:
{task_description}

各ステップは具体的で、実行可能な形式にしてください。
結果として期待されるものも明記してください。
"""
            # LLMでToDoリストを生成
            ollama_llm = ChatOllama(model=OLLAMA_MODEL, temperature=0.3)
            response = await asyncio.to_thread(
                lambda: ollama.chat(
                    model=OLLAMA_MODEL,
                    messages=[
                        {
                            "role": "system",
                            "content": "あなたはタスクを具体的なToDoリストに分解するアシスタントです。",
                        },
                        {"role": "user", "content": todo_prompt},
                    ],
                )
            )

            todo_list = response["message"]["content"]

            # ToDoリストを表示
            await status_updater(f"タスク分析結果:\n\n{todo_list}", is_thinking=True)
            return todo_list

        # 非同期関数でブラウザタスクを実行
        async def run_browser_task():
            # コンテキスト情報を取得
            context = get_context_info()

            # タスクをToDoリストに変換
            todo_list = await create_todo_list(task)
            say(f"📋 *計画を立てました*\n\n{todo_list}", thread_ts=thread_ts)

            # タスクにコンテキスト情報を追加
            enhanced_task = f"""
現在の日時: {context['current_datetime']} ({context['day_of_week']})
現在位置: {context['location']}

以下のタスクを実行してください:
{task}

タスク実行計画:
{todo_list}

各ステップを実行する際に、以下のフレームワークに従ってください:
1. 思考: 何をしようとしているか、なぜそれが必要か
2. 行動: 具体的に何のアクションを取るか
3. 観察: 行動の結果何が得られたか
4. 次のステップ: 結果に基づいて次に何をするか

各ステップで思考プロセスを詳細に説明し、段階的に進めてください。
途中結果を常に報告してください。
"""

            # 進捗状況を更新
            await status_updater("タスクの実行を開始します", is_thinking=False)

            # OllamaのLLMを作成
            ollama_llm = ChatOllama(model=OLLAMA_MODEL, temperature=0.7)

            # カスタムコールバック関数を作成
            class ThoughtTrackingCallbackHandler:
                def on_llm_start(self, serialized, prompts, **kwargs):
                    asyncio.create_task(
                        status_updater(
                            "思考プロセスを開始しています...", is_thinking=True
                        )
                    )

                def on_llm_end(self, response, **kwargs):
                    # 思考プロセスを抽出
                    thought = (
                        response.generations[0][0].text
                        if hasattr(response, "generations")
                        else str(response)
                    )
                    # 思考プロセスが長すぎる場合は要約
                    if len(thought) > 200:
                        thought = thought[:197] + "..."
                    asyncio.create_task(status_updater(thought, is_thinking=True))

                def on_tool_start(self, serialized, input_str, **kwargs):
                    tool_name = serialized.get("name", "不明なツール")
                    asyncio.create_task(
                        status_updater(
                            f"ツール '{tool_name}' を使用: {input_str}",
                            is_thinking=False,
                        )
                    )

                def on_tool_end(self, output, **kwargs):
                    # 出力が長すぎる場合は要約
                    if isinstance(output, str) and len(output) > 200:
                        output = output[:197] + "..."
                    asyncio.create_task(
                        status_updater(f"結果: {output}", is_thinking=False)
                    )

            # BrowserAgentを作成
            try:
                # 進捗状況を更新
                await status_updater(
                    "エージェントを初期化しています...", is_thinking=False
                )

                agent = BrowserAgent(
                    task=enhanced_task,
                    llm=ollama_llm,
                )

                # タスク実行開始メッセージ
                say("🚀 *タスク実行を開始しました*", thread_ts=thread_ts)

                # タスクを実行
                result = await agent.run()

                # 結果を整形して表示
                formatted_result = format_browser_result(result, thought_process)

                # 結果のサマリーを生成
                summary_prompt = f"""
以下はタスク「{task}」の実行結果です:

{formatted_result}

この結果を簡潔に要約し、重要なポイントをまとめてください。
タスクの目的が達成されたかどうかも評価してください。
"""

                await status_updater("結果を要約しています...", is_thinking=True)

                summary_response = ollama.chat(
                    model=OLLAMA_MODEL,
                    messages=[
                        {
                            "role": "system",
                            "content": "あなたはタスク実行結果を要約するアシスタントです。",
                        },
                        {"role": "user", "content": summary_prompt},
                    ],
                )

                summary = summary_response["message"]["content"]

                # 最終結果を表示
                say(
                    f"✅ *タスク完了*\n\n{formatted_result}\n\n📝 *要約*\n\n{summary}",
                    thread_ts=thread_ts,
                )

            except Exception as agent_error:
                error_msg = f"エージェントの実行に失敗しました: {str(agent_error)}"
                logger.error(error_msg, exc_info=True)
                say(f"❌ *エラー*\n\n{error_msg}", thread_ts=thread_ts)
                return

            # 記録ファイルをアップロード
            try:
                # 録画ファイルを探す
                recording_dir = os.path.join(
                    tempfile.gettempdir(), "browser_use_recordings"
                )
                os.makedirs(recording_dir, exist_ok=True)
                recording_files = []
                for file in os.listdir(recording_dir):
                    if file.endswith(".gif") or file.endswith(".mp4"):
                        recording_files.append(os.path.join(recording_dir, file))

                if recording_files:
                    # 最新の記録ファイルを使用
                    recording_files.sort(key=os.path.getmtime, reverse=True)
                    latest_recording = recording_files[0]

                    # Slackにアップロード
                    from slack_sdk.web import WebClient

                    client = WebClient(token=SLACK_BOT_TOKEN)

                    await status_updater(
                        "タスク実行の記録をアップロードしています...", is_thinking=False
                    )

                    response = client.files_upload_v2(
                        file=latest_recording,
                        title=f"タスク実行: {task[:50]}...",
                        initial_comment="🎬 タスク実行の記録",
                        thread_ts=thread_ts,
                        channel=channel_id,
                    )
                    logger.info(f"記録のアップロード成功: {response}")
                else:
                    say(
                        "📝 タスク実行の記録が見つかりませんでした。",
                        thread_ts=thread_ts,
                    )
            except Exception as upload_error:
                logger.error(f"記録のアップロード失敗: {str(upload_error)}")
                say(
                    f"⚠️ 記録のアップロードに失敗しました: {str(upload_error)}",
                    thread_ts=thread_ts,
                )

            # ユーザーのAIインスタンスに会話を保存
            ai.save_conversation(
                f"[browser-use] {task}",
                f"タスク実行結果: {formatted_result}",
                None,  # 新しい会話IDを生成
            )

        # 非同期関数を実行
        try:
            loop = asyncio.new_event_loop()
            asyncio.set_event_loop(loop)
            loop.run_until_complete(run_browser_task())
            loop.close()
        except Exception as loop_error:
            error_msg = f"非同期処理でエラーが発生しました: {str(loop_error)}"
            logger.error(error_msg, exc_info=True)
            say(f"❌ *エラー*\n\n{error_msg}", thread_ts=thread_ts)

    except Exception as e:
        error_msg = f"タスク実行中にエラーが発生しました: {str(e)}"
        logger.error(error_msg, exc_info=True)
        say(f"❌ *エラー*\n\n{error_msg}", thread_ts=thread_ts)


def format_browser_result(result, thought_process=None):
    """
    ブラウザタスクの結果を整形する

    Args:
        result: ブラウザタスクの実行結果
        thought_process: 思考プロセスのリスト

    Returns:
        整形された結果テキスト
    """
    try:
        # 思考プロセスの整形
        thought_section = ""
        if thought_process and len(thought_process) > 0:
            thought_section = "*思考プロセス*\n"
            for i, thought in enumerate(thought_process):
                thought_section += f"{i+1}. {thought}\n"
            thought_section += "\n"

        # 結果が文字列の場合
        if isinstance(result, str):
            return f"{thought_section}*結果*\n{result}"

        # AgentHistoryListの場合
        if hasattr(result, "all_results"):
            formatted_text = ""

            if thought_section:
                formatted_text += thought_section

            formatted_text += "*実行結果*\n\n"

            # 実行ステップを収集
            all_steps = []
            for i, action in enumerate(result.all_results):
                step_info = {}

                # アクション情報
                step_info["index"] = i + 1
                step_info["content"] = (
                    action.extracted_content if action.extracted_content else None
                )
                step_info["error"] = action.error if action.error else None
                step_info["success"] = action.success

                all_steps.append(step_info)

            # 成功したステップを表示
            successful_steps = [
                step for step in all_steps if step["content"] and not step["error"]
            ]
            if successful_steps:
                formatted_text += "*成功したステップ:*\n"
                for step in successful_steps:
                    formatted_text += f"{step['index']}. {step['content']}\n"
                formatted_text += "\n"

            # エラーステップを表示
            error_steps = [step for step in all_steps if step["error"]]
            if error_steps:
                formatted_text += "*エラーが発生したステップ:*\n"
                for step in error_steps:
                    formatted_text += f"{step['index']}. エラー: {step['error']}\n"
                formatted_text += "\n"

            # モデル出力を処理
            if hasattr(result, "all_model_outputs") and result.all_model_outputs:
                formatted_text += "*詳細な出力:*\n"
                for i, output in enumerate(result.all_model_outputs):
                    if isinstance(output, dict):
                        formatted_text += f"ステップ {i+1}:\n"
                        for key, value in output.items():
                            if value:
                                if isinstance(value, dict):
                                    formatted_text += f"• {key}:\n"
                                    for k, v in value.items():
                                        formatted_text += f"  - {k}: {v}\n"
                                else:
                                    formatted_text += f"• {key}: {value}\n"
                        formatted_text += "\n"

            # 収集した情報の要約
            extracted_info = []
            for step in all_steps:
                if step["content"] and isinstance(step["content"], str):
                    if ":" in step["content"]:
                        parts = step["content"].split(":", 1)
                        if len(parts) == 2:
                            key, value = parts[0].strip(), parts[1].strip()
                            if key and value and len(value) > 5:  # 有用な情報のみ抽出
                                extracted_info.append(f"• {key}: {value}")

            if extracted_info:
                formatted_text += "*収集した情報:*\n"
                formatted_text += "\n".join(extracted_info)
                formatted_text += "\n\n"

            return formatted_text

        # その他の型の場合は文字列に変換
        return f"{thought_section}*結果*\n{str(result)}"

    except Exception as e:
        logger.error(f"結果の整形中にエラー: {str(e)}")
        return f"*結果の整形中にエラーが発生しました*\n{str(e)}\n\n*元の結果:*\n{str(result)}"


def start_slack_bot():
    """Slackボットを起動"""
    if not SLACK_BOT_TOKEN or not SLACK_APP_TOKEN:
        logger.error("環境変数 SLACK_BOT_TOKEN と SLACK_APP_TOKEN が必要です。")
        logger.info("以下の内容で .env ファイルを作成してください：")
        logger.info("SLACK_BOT_TOKEN=xoxb-your-bot-token")
        logger.info("SLACK_APP_TOKEN=xapp-your-app-token")
        logger.info("OLLAMA_MODEL=llama3.3")
        logger.info("IMAGE_MODEL=gemma3:27b")
        logger.info("OLLAMA_HOST=http://localhost:11434")
        return False

    logger.info(f"ボット設定: テキストモデル={OLLAMA_MODEL}, 画像モデル={IMAGE_MODEL}")

    # Ollamaサーバーをチェック
    try:
        response = requests.get(f"{OLLAMA_HOST}/api/tags")
        if response.status_code == 200:
            available_models = response.json()
            model_names = []
            for model in available_models.get("models", []):
                if isinstance(model, dict) and "name" in model:
                    model_names.append(model["name"])

            logger.info(f"利用可能なOllamaモデル: {model_names}")

            # マルチモーダルモデルの存在チェック
            multimodal_models = [
                model
                for model in model_names
                if model in FALLBACK_IMAGE_MODELS or model == IMAGE_MODEL
            ]
            if multimodal_models:
                logger.info(f"使用可能なマルチモーダルモデル: {multimodal_models}")
            else:
                logger.warning(
                    f"マルチモーダルモデルが見つかりません。llava のインストールをお勧めします: ollama pull llava"
                )
        else:
            logger.warning(
                f"Ollamaサーバーからのレスポンスが不正: {response.status_code} - {response.text}"
            )
    except Exception as e:
        logger.warning(f"Ollamaサーバーへの接続に失敗しました: {str(e)}")

    # デバッグモードの表示
    if DEBUG_MODE:
        logger.info(
            f"デバッグモードが有効です。画像ファイルは {DEBUG_IMAGES_DIR} ディレクトリに保存されます。"
        )
    else:
        logger.info(f"デバッグモードは無効です。画像ファイルは処理後に削除されます。")

    # MCPコントローラの初期化（非同期なので別スレッドで実行）
    if MCP_AVAILABLE:
        logger.info("MCPコントローラを初期化しています...")
        
        def init_mcp_controller():
            loop = asyncio.new_event_loop()
            asyncio.set_event_loop(loop)
            try:
                loop.run_until_complete(mcp_terminal_controller.initialize())
                logger.info("MCPコントローラの初期化が完了しました")
            except Exception as e:
                logger.error(f"MCPコントローラの初期化に失敗: {str(e)}", exc_info=True)
            finally:
                loop.close()
        
        # 別スレッドでMCPコントローラを初期化
        mcp_init_thread = threading.Thread(target=init_mcp_controller, daemon=True)
        mcp_init_thread.start()
    
    try:
        logger.info("Slackボットを起動中...")

        # ボットを起動したらステータスを「起動中！」に設定
        set_bot_status(
            status_text="起動中！", status_emoji=":robot_face:", presence="auto"
        )

        handler = SocketModeHandler(app, SLACK_APP_TOKEN)
        logger.info("Socket Modeハンドラを作成しました")
        logger.info("接続を開始します...")
        handler.start()
        return True
    except Exception as e:
        logger.error(
            f"Slackボットの起動中にエラーが発生しました: {str(e)}", exc_info=True
        )
        return False


def start_slack_bot_in_background():
    """
    CLIからSlackボットをバックグラウンドスレッドで起動する
    """
    if not SLACK_BOT_TOKEN or not SLACK_APP_TOKEN:
        logger.warning("Slackボットの起動に必要な環境変数が設定されていません")
        return False

    logger.info("Slackボットをバックグラウンドで起動します...")

    # MCPコントローラの初期化（非同期なので別スレッドで実行）
    if MCP_AVAILABLE:
        logger.info("MCPコントローラを初期化しています...")
        
        def init_mcp_controller():
            loop = asyncio.new_event_loop()
            asyncio.set_event_loop(loop)
            try:
                loop.run_until_complete(mcp_terminal_controller.initialize())
                logger.info("MCPコントローラの初期化が完了しました")
            except Exception as e:
                logger.error(f"MCPコントローラの初期化に失敗: {str(e)}", exc_info=True)
            finally:
                loop.close()
        
        # 別スレッドでMCPコントローラを初期化
        mcp_init_thread = threading.Thread(target=init_mcp_controller, daemon=True)
        mcp_init_thread.start()
    
    def run_bot():
        try:
            # ボットを起動したらステータスを「起動中！」に設定
            set_bot_status(
                status_text="起動中！", status_emoji=":robot_face:", presence="auto"
            )

            handler = SocketModeHandler(app, SLACK_APP_TOKEN)
            logger.info("Socket Modeハンドラを作成しました")
            handler.start()
        except Exception as e:
            logger.error(
                f"バックグラウンドでのSlackボット起動に失敗: {str(e)}", exc_info=True
            )

    # バックグラウンドスレッドでボットを起動
    bot_thread = threading.Thread(target=run_bot, daemon=True)
    bot_thread.start()
    logger.info("Slackボットをバックグラウンドで起動しました")
    return True


if __name__ == "__main__":
    start_slack_bot()
