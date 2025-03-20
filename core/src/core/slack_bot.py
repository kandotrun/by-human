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
        # 新しいAIインスタンスを作成
        logger.info(
            f"ユーザー {user_id} の新しいAIインスタンスを作成します (モデル: {OLLAMA_MODEL})"
        )
        user_ai_instances[user_id] = PersonalAI(
            user_id=f"slack_{user_id}", ollama_model=OLLAMA_MODEL
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


def handle_help_command(ai_instance: PersonalAI) -> str:
    """
    ヘルプコマンドの処理

    Args:
        ai_instance: PersonalAIインスタンス

    Returns:
        ヘルプメッセージ
    """
    help_text = ai_instance._handle_help([])
    return f"*使い方* \n{help_text}\n*注意*: コマンドは '/' から始めるか、'!'から始めてください。"


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

        # プロンプトが指定されている場合は、CSVの情報とプロンプトを使ってOllamaに問い合わせ
        if prompt:
            logger.info(f"CSVファイル解析: Ollamaに問い合わせ - {prompt}")

            # LLMを使用して解析
            response = ollama.chat(
                model=OLLAMA_MODEL,
                messages=[
                    {
                        "role": "system",
                        "content": f"あなたはCSVデータの解析と説明を行うアシスタントです。以下のCSVファイルの情報に基づいて質問に答えてください。\n\n{csv_info}",
                    },
                    {"role": "user", "content": prompt},
                ],
            )

            result = response["message"]["content"]
            return result
        else:
            # プロンプトがない場合はCSV情報のみを返す
            return f"CSVファイルの解析結果:\n\n{csv_info}"

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

            # LLMを使用して解析
            response = ollama.chat(
                model=OLLAMA_MODEL,
                messages=[
                    {
                        "role": "system",
                        "content": f"あなたはPDFドキュメントの解析と要約を行うアシスタントです。以下のPDFの情報に基づいて質問に答えてください。\n\n{pdf_info}",
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
            return f"PDFファイルの解析結果:\n\nページ数: {page_count}\n\n内容の一部:\n{short_content}"

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

        # 画像をbase64エンコード
        with open(image_path, "rb") as img_file:
            image_data = img_file.read()
            base64_image = base64.b64encode(image_data).decode("utf-8")

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
                    prompt=prompt,
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

    # 画像表示リクエストの処理（優先的に処理）
    if text.startswith("/image") or is_image_request(text):
        logger.info(f"画像リクエスト検出: '{text}'")
        # スレッドのチャンネルIDを取得
        channel_id = None
        if thread_ts:
            try:
                from slack_sdk.web import WebClient

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
    try:
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
        return

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

    try:
        logger.info("Slackボットを起動中...")
        handler = SocketModeHandler(app, SLACK_APP_TOKEN)
        logger.info("Socket Modeハンドラを作成しました")
        logger.info("接続を開始します...")
        handler.start()
    except Exception as e:
        logger.error(
            f"Slackボットの起動中にエラーが発生しました: {str(e)}", exc_info=True
        )


if __name__ == "__main__":
    start_slack_bot()
