"""
Markdownファイルを記憶として使用するパーソナライズAI実装 - Ollama Llama 3.3版

このスクリプトは、LangChainとOllamaのLlama 3.3モデルを使用して以下の機能を持つパーソナルAIを実装します：
1. コンテキスト理解と記憶機能（Markdownファイルに保存）
2. 基本的なタスク自動化機能
3. 会話ベースの自動フィードバック収集と学習メカニズム
"""

import os
import re
import json
import datetime
import subprocess
import warnings
import io
import sys
from contextlib import redirect_stderr, redirect_stdout
from typing import List, Dict, Any, Optional, Tuple

# 警告メッセージを抑制
warnings.filterwarnings("ignore", category=DeprecationWarning)
warnings.filterwarnings("ignore", message=".*The class.*LLMChain.*was deprecated.*")
warnings.filterwarnings("ignore", message=".*The method.*Chain.run.*was deprecated.*")
warnings.filterwarnings(
    "ignore", message=".*The class.*OllamaEmbeddings.*was deprecated.*"
)
warnings.filterwarnings("ignore", message=".*The class.*Chroma.*was deprecated.*")
warnings.filterwarnings("ignore", category=SyntaxWarning, module="geocoder")

import geocoder
from langchain_ollama import OllamaLLM, OllamaEmbeddings
from langchain_chroma import Chroma
from langchain.chains import LLMChain
from langchain.prompts import PromptTemplate
from langchain.tools import BaseTool
from langchain.callbacks import StdOutCallbackHandler
from rich.console import Console
from rich.panel import Panel
from rich.markdown import Markdown
from rich.table import Table
from rich.prompt import Prompt, Confirm
from rich.syntax import Syntax


class MarkdownFileTool(BaseTool):
    """Markdownファイルを操作するためのツールクラス"""

    name: str = "markdown_file_tool"
    description: str = "Markdownファイルの読み書きを行うツール"

    def _run(self, action: str, file_path: str, content: str = None) -> str:
        """
        ツールの実行

        Args:
            action: 実行するアクション（read/write/append）
            file_path: 操作するファイルのパス
            content: 書き込む内容（writeとappendの場合）

        Returns:
            実行結果
        """
        # ディレクトリが存在しない場合は作成
        os.makedirs(os.path.dirname(file_path), exist_ok=True)

        if action == "read":
            if not os.path.exists(file_path):
                return ""

            with open(file_path, "r", encoding="utf-8") as f:
                return f.read()

        elif action == "write":
            with open(file_path, "w", encoding="utf-8") as f:
                f.write(content)
            return f"ファイル {file_path} に書き込みました"

        elif action == "append":
            with open(file_path, "a", encoding="utf-8") as f:
                f.write(content)
            return f"ファイル {file_path} に追記しました"

        else:
            return f"不明なアクション: {action}"

    def _arun(self, query: str):
        raise NotImplementedError("このツールは非同期実行をサポートしていません")


class VectorMemory:
    """会話や情報をベクトル化して保存・検索するためのクラス"""

    def __init__(
        self,
        user_id: str,
        ollama_model: str = "llama3.3",
        persist_directory: str = "vector_memories",
    ):
        """
        初期化

        Args:
            user_id: ユーザーの一意識別子
            ollama_model: 使用するOllamaモデル名
            persist_directory: ベクトルデータの保存先ディレクトリ
        """
        self.user_id = user_id
        self.persist_directory = f"{persist_directory}/{user_id}"

        # エンベディングモデルの初期化
        self.embeddings = OllamaEmbeddings(model=ollama_model)

        # ベクトルストアの初期化
        self._initialize_vector_store()

    def _initialize_vector_store(self):
        """ベクトルストアを初期化（存在する場合はロード、存在しない場合は新規作成）"""
        os.makedirs(self.persist_directory, exist_ok=True)

        try:
            # 既存のベクトルストアをロード
            self.vector_store = Chroma(
                persist_directory=self.persist_directory,
                embedding_function=self.embeddings,
            )
        except Exception:
            # 新規作成
            self.vector_store = Chroma(
                persist_directory=self.persist_directory,
                embedding_function=self.embeddings,
            )

    def add_texts(
        self, texts: List[str], metadatas: Optional[List[Dict[str, Any]]] = None
    ) -> List[str]:
        """
        テキストをベクトル化して保存

        Args:
            texts: ベクトル化するテキストのリスト
            metadatas: テキストに関連するメタデータのリスト

        Returns:
            保存されたドキュメントのID
        """
        return self.vector_store.add_texts(texts=texts, metadatas=metadatas)

    def add_conversation(
        self, human_input: str, ai_output: str, conversation_id: str, timestamp: str
    ) -> List[str]:
        """
        会話をベクトル化して保存

        Args:
            human_input: ユーザーの入力
            ai_output: AIの応答
            conversation_id: 会話ID
            timestamp: タイムスタンプ

        Returns:
            保存されたドキュメントのID
        """
        # 会話全体を1つのテキストとしてベクトル化
        combined_text = f"ユーザー: {human_input}\nAI: {ai_output}"

        # メタデータの設定
        metadata = {
            "type": "conversation",
            "conversation_id": conversation_id,
            "timestamp": timestamp,
            "human_input": human_input,
            "ai_output": ai_output,
        }

        return self.add_texts([combined_text], [metadata])

    def add_profile_info(self, key: str, value: str) -> List[str]:
        """
        プロファイル情報をベクトル化して保存

        Args:
            key: プロファイル情報のキー
            value: プロファイル情報の値

        Returns:
            保存されたドキュメントのID
        """
        # プロファイル情報をテキスト化
        text = f"{key}: {value}"

        # メタデータの設定
        metadata = {
            "type": "profile",
            "key": key,
            "value": value,
            "timestamp": datetime.datetime.now().strftime("%Y-%m-%d %H:%M:%S"),
        }

        return self.add_texts([text], [metadata])

    def search(
        self, query: str, k: int = 5, filter: Optional[Dict[str, Any]] = None
    ) -> List[Dict[str, Any]]:
        """
        関連する記憶を検索

        Args:
            query: 検索クエリ
            k: 取得する結果の最大数
            filter: フィルタリング条件

        Returns:
            関連する記憶のリスト
        """
        documents = self.vector_store.similarity_search(query=query, k=k, filter=filter)

        results = []
        for doc in documents:
            result = {"content": doc.page_content, "metadata": doc.metadata}
            results.append(result)

        return results

    def search_conversations(self, query: str, k: int = 5) -> List[Dict[str, Any]]:
        """
        関連する会話を検索

        Args:
            query: 検索クエリ
            k: 取得する結果の最大数

        Returns:
            関連する会話のリスト
        """
        return self.search(query, k, {"type": "conversation"})

    def search_profile(self, query: str, k: int = 5) -> List[Dict[str, Any]]:
        """
        関連するプロファイル情報を検索

        Args:
            query: 検索クエリ
            k: 取得する結果の最大数

        Returns:
            関連するプロファイル情報のリスト
        """
        return self.search(query, k, {"type": "profile"})

    def clear(self) -> None:
        """ベクトルストアを削除"""
        self.vector_store.delete_collection()
        self._initialize_vector_store()


class MarkdownMemory:
    """Markdownファイルを使用して会話履歴と個人情報を保存するカスタムメモリクラス"""

    def __init__(
        self,
        user_id: str,
        memory_dir: str = "memories",
        file_tool: MarkdownFileTool = None,
        vector_memory: VectorMemory = None,
        ollama_model: str = "llama3.3",
    ):
        """
        初期化

        Args:
            user_id: ユーザーの一意識別子
            memory_dir: 記憶を保存するディレクトリ
            file_tool: ファイル操作ツール
            vector_memory: ベクトルメモリ（Noneの場合は新規作成）
            ollama_model: 使用するOllamaモデル名
        """
        self.user_id = user_id
        self.memory_dir = memory_dir
        self.conversations_dir = f"{memory_dir}/{user_id}_conversations"
        self.profile_file = f"{memory_dir}/{user_id}_profile.md"
        self.preferences_file = f"{memory_dir}/{user_id}_preferences.md"
        self.feedback_file = f"{memory_dir}/{user_id}_feedback.md"
        self.satisfaction_file = f"{memory_dir}/{user_id}_satisfaction.md"
        self.images_dir = f"{memory_dir}/{user_id}_images"  # 画像保存用ディレクトリ
        self.images_metadata_file = (
            f"{memory_dir}/{user_id}_images_metadata.md"  # 画像メタデータ保存用ファイル
        )

        # ファイル操作ツール
        self.file_tool = file_tool if file_tool else MarkdownFileTool()

        # ベクトルメモリ
        self.vector_memory = (
            vector_memory if vector_memory else VectorMemory(user_id, ollama_model)
        )

        # ディレクトリが存在しない場合は作成
        os.makedirs(memory_dir, exist_ok=True)
        os.makedirs(self.conversations_dir, exist_ok=True)
        os.makedirs(self.images_dir, exist_ok=True)  # 画像ディレクトリを作成

        # 各ファイルが存在しない場合は作成
        for file_path, header in [
            (self.profile_file, "# ユーザープロファイル\n\n"),
            (self.preferences_file, "# ユーザー設定\n\n"),
            (self.feedback_file, "# フィードバック履歴\n\n"),
            (self.satisfaction_file, "# 満足度履歴\n\n"),
            (
                self.images_metadata_file,
                "# 保存された画像一覧\n\n",
            ),  # 画像メタデータファイル
        ]:
            if not os.path.exists(file_path):
                self.file_tool._run("write", file_path, header)

        # 今日の会話ファイルが存在しない場合は作成
        today_file = self._get_conversation_file_for_date(datetime.datetime.now())
        if not os.path.exists(today_file):
            self.file_tool._run(
                "write",
                today_file,
                f"# {datetime.datetime.now().strftime('%Y年%m月%d日')}の会話履歴\n\n",
            )

    def _get_conversation_file_for_date(self, date: datetime.datetime) -> str:
        """
        指定された日付の会話ファイルパスを取得

        Args:
            date: 日付

        Returns:
            ファイルパス
        """
        date_str = date.strftime("%Y-%m-%d")
        return f"{self.conversations_dir}/{date_str}.md"

    def save_conversation(
        self, human_input: str, ai_output: str, conversation_id: str = None
    ) -> str:
        """
        会話をMarkdownファイルに保存

        Args:
            human_input: ユーザーの入力
            ai_output: AIの応答
            conversation_id: 会話ID（Noneの場合は新しいIDを生成）

        Returns:
            conversation_id: 会話ID
        """
        timestamp = datetime.datetime.now().strftime("%Y-%m-%d %H:%M:%S")

        # 会話IDがない場合は生成
        if conversation_id is None:
            conversation_id = f"conv_{timestamp.replace(' ', '_').replace(':', '-')}"

        content = f"## {timestamp} - {conversation_id}\n\n"
        content += f"### ユーザー\n\n{human_input}\n\n"
        content += f"### AI\n\n{ai_output}\n\n"
        content += "---\n\n"

        # 現在の日付のファイルに保存
        current_date = datetime.datetime.now()
        conversation_file = self._get_conversation_file_for_date(current_date)

        # ファイルが存在しない場合は作成
        if not os.path.exists(conversation_file):
            header = f"# {current_date.strftime('%Y年%m月%d日')}の会話履歴\n\n"
            self.file_tool._run("write", conversation_file, header)

        self.file_tool._run("append", conversation_file, content)

        # ベクトルメモリにも保存
        try:
            self.vector_memory.add_conversation(
                human_input, ai_output, conversation_id, timestamp
            )
        except Exception as e:
            print(f"ベクトルメモリへの保存に失敗しました: {str(e)}")

        return conversation_id

    def save_profile_info(self, key: str, value: str) -> None:
        """ユーザープロファイル情報を保存または更新"""
        # 既存のプロファイルを読み込む
        profile_content = self.file_tool._run("read", self.profile_file)

        # キーが既に存在するか確認
        key_exists = False
        lines = profile_content.split("\n")
        for i, line in enumerate(lines):
            if line.startswith(f"- **{key}**:"):
                lines[i] = f"- **{key}**: {value}"
                key_exists = True
                break

        # キーが存在しない場合は追加
        if not key_exists:
            if "# ユーザープロファイル" in profile_content:
                lines.append(f"- **{key}**: {value}")
            else:
                lines = ["# ユーザープロファイル\n", f"- **{key}**: {value}"]

        # 更新された内容を書き込む
        self.file_tool._run("write", self.profile_file, "\n".join(lines))

        # ベクトルメモリにも保存
        try:
            self.vector_memory.add_profile_info(key, value)
        except Exception as e:
            print(f"ベクトルメモリへの保存に失敗しました: {str(e)}")

    def save_preference(self, category: str, key: str, value: str) -> None:
        """ユーザー設定を保存または更新"""
        # 既存の設定を読み込む
        preferences_content = self.file_tool._run("read", self.preferences_file)

        # カテゴリとキーが既に存在するか確認
        lines = preferences_content.split("\n")
        category_exists = False
        key_exists = False
        category_index = -1

        for i, line in enumerate(lines):
            if line.startswith(f"## {category}"):
                category_exists = True
                category_index = i
            elif category_exists and line.startswith(f"- **{key}**:"):
                lines[i] = f"- **{key}**: {value}"
                key_exists = True
                break

        # 更新または追加
        if not category_exists:
            if "# ユーザー設定" in preferences_content:
                lines.append(f"\n## {category}\n")
                lines.append(f"- **{key}**: {value}")
            else:
                lines = [
                    "# ユーザー設定\n",
                    f"## {category}\n",
                    f"- **{key}**: {value}",
                ]
        elif not key_exists:
            # カテゴリは存在するがキーが存在しない場合
            # カテゴリの後に挿入
            insert_index = category_index + 1
            while insert_index < len(lines) and not lines[insert_index].startswith(
                "##"
            ):
                insert_index += 1
            lines.insert(insert_index, f"- **{key}**: {value}")

        # 更新された内容を書き込む
        self.file_tool._run("write", self.preferences_file, "\n".join(lines))

    def save_feedback(
        self,
        conversation_id: str,
        feedback_type: str,
        feedback_content: str,
        original_response: str = None,
        improved_response: str = None,
    ) -> None:
        """
        ユーザーフィードバックを保存

        Args:
            conversation_id: フィードバック対象の会話ID
            feedback_type: フィードバックの種類（明示的/暗黙的/感情/提案など）
            feedback_content: フィードバックの内容
            original_response: 元の応答（オプション）
            improved_response: 改善された応答（オプション）
        """
        timestamp = datetime.datetime.now().strftime("%Y-%m-%d %H:%M:%S")

        content = f"## フィードバック {timestamp} - {conversation_id}\n\n"
        content += f"### タイプ\n\n{feedback_type}\n\n"
        content += f"### 内容\n\n{feedback_content}\n\n"

        if original_response:
            content += f"### 元の応答\n\n{original_response}\n\n"

        if improved_response:
            content += f"### 改善された応答\n\n{improved_response}\n\n"

        content += "---\n\n"

        self.file_tool._run("append", self.feedback_file, content)

    def save_satisfaction(
        self, conversation_id: str, satisfaction_level: int, comment: str = None
    ) -> None:
        """
        ユーザー満足度を保存

        Args:
            conversation_id: 満足度評価対象の会話ID
            satisfaction_level: 満足度レベル（1-5）
            comment: コメント（オプション）
        """
        timestamp = datetime.datetime.now().strftime("%Y-%m-%d %H:%M:%S")

        content = f"## 満足度 {timestamp} - {conversation_id}\n\n"
        content += f"### レベル\n\n{satisfaction_level}/5\n\n"

        if comment:
            content += f"### コメント\n\n{comment}\n\n"

        content += "---\n\n"

        self.file_tool._run("append", self.satisfaction_file, content)

    def get_recent_conversations(self, n: int = 5) -> str:
        """最近のn件の会話を取得"""
        # 日付の降順で会話ファイルを取得
        conversation_files = []
        if os.path.exists(self.conversations_dir):
            for file_name in sorted(os.listdir(self.conversations_dir), reverse=True):
                if file_name.endswith(".md"):
                    conversation_files.append(
                        os.path.join(self.conversations_dir, file_name)
                    )

        all_conversations = []
        remaining_count = n

        # 最新のファイルから順に会話を取得
        for file_path in conversation_files:
            if remaining_count <= 0:
                break

            content = self.file_tool._run("read", file_path)
            if not content:
                continue

            # ヘッダーを除去
            if "# " in content and "\n\n" in content:
                content = content.split("\n\n", 1)[1]

            # 会話を分割（"---"で区切られている）
            conversations = content.split("---")

            # 空の要素を除去
            conversations = [conv for conv in conversations if conv.strip()]

            # 最新のものから追加
            for conv in reversed(conversations):
                if remaining_count <= 0:
                    break
                all_conversations.append(conv)
                remaining_count -= 1

        # 取得した順（新しい順）に並べ替え
        all_conversations.reverse()

        if not all_conversations:
            return ""

        return "---\n\n".join(all_conversations) + "---\n\n"

    def get_profile(self) -> str:
        """ユーザープロファイルを取得"""
        return self.file_tool._run("read", self.profile_file)

    def get_preferences(self) -> str:
        """ユーザー設定を取得"""
        return self.file_tool._run("read", self.preferences_file)

    def get_feedback_patterns(self) -> str:
        """フィードバックパターンを取得"""
        return self.file_tool._run("read", self.feedback_file)

    def get_satisfaction_trends(self) -> str:
        """満足度傾向を取得"""
        return self.file_tool._run("read", self.satisfaction_file)

    def search_related_memories(self, query: str, k: int = 3) -> str:
        """
        質問に関連する記憶を検索

        Args:
            query: 検索クエリ
            k: 取得する記憶の数

        Returns:
            関連する記憶のテキスト表現
        """
        try:
            # 関連する会話とプロファイル情報を検索
            conversations = self.vector_memory.search_conversations(query, k)
            profiles = self.vector_memory.search_profile(query, k)

            result = ""

            # プロファイル情報を追加
            if profiles:
                result += "## 関連するプロファイル情報\n\n"
                for i, profile in enumerate(profiles):
                    result += f"{i+1}. {profile['content']}\n"
                result += "\n"

            # 会話を追加
            if conversations:
                result += "## 関連する過去の会話\n\n"
                for i, conv in enumerate(conversations):
                    human = conv["metadata"].get("human_input", "")
                    ai = conv["metadata"].get("ai_output", "")
                    timestamp = conv["metadata"].get("timestamp", "")

                    result += f"{i+1}. {timestamp}\n"
                    result += f"   ユーザー: {human}\n"
                    result += f"   AI: {ai}\n\n"

            return result
        except Exception as e:
            print(f"記憶の検索に失敗しました: {str(e)}")
            return ""

    def save_image(
        self, image_path: str, title: str, description: str = "", tags: List[str] = None
    ) -> str:
        """
        画像を保存し、メタデータを記録

        Args:
            image_path: 元の画像ファイルのパス
            title: 画像のタイトル
            description: 画像の説明
            tags: 画像に関連するタグのリスト

        Returns:
            保存された画像のID
        """
        # 画像IDの生成（タイムスタンプベース）
        timestamp = datetime.datetime.now().strftime("%Y%m%d_%H%M%S")
        image_id = f"img_{timestamp}"

        # 画像の拡張子を取得
        _, ext = os.path.splitext(image_path)
        if not ext:
            ext = ".jpg"  # デフォルトの拡張子

        # 保存先のパスを決定
        destination_path = os.path.join(self.images_dir, f"{image_id}{ext}")

        # 画像をコピー
        import shutil

        try:
            shutil.copy2(image_path, destination_path)
        except Exception as e:
            print(f"画像の保存に失敗しました: {str(e)}")
            return ""

        # タグの処理
        tags_str = ", ".join(tags) if tags else ""

        # メタデータをMarkdownファイルに追加
        content = f"## {image_id} - {title}\n\n"
        content += f"- **ファイル名**: {os.path.basename(destination_path)}\n"
        content += (
            f"- **日時**: {datetime.datetime.now().strftime('%Y-%m-%d %H:%M:%S')}\n"
        )
        if description:
            content += f"- **説明**: {description}\n"
        if tags_str:
            content += f"- **タグ**: {tags_str}\n"
        content += "\n---\n\n"

        # メタデータを保存
        self.file_tool._run("append", self.images_metadata_file, content)

        # ベクトルメモリに画像情報を保存（検索用）
        try:
            metadata = {
                "type": "image",
                "image_id": image_id,
                "title": title,
                "description": description,
                "tags": tags_str,
                "file_path": destination_path,
                "timestamp": datetime.datetime.now().strftime("%Y-%m-%d %H:%M:%S"),
            }
            self.vector_memory.add_texts(
                [f"{title} {description} {tags_str}"], [metadata]
            )
        except Exception as e:
            print(f"ベクトルメモリへの画像情報保存に失敗しました: {str(e)}")

        return image_id

    def get_image_path(self, image_id: str) -> str:
        """
        画像IDから画像ファイルのパスを取得

        Args:
            image_id: 画像ID

        Returns:
            画像ファイルのパス（見つからない場合は空文字列）
        """
        # 画像ディレクトリ内のファイルを検索
        for file_name in os.listdir(self.images_dir):
            if file_name.startswith(image_id):
                return os.path.join(self.images_dir, file_name)

        return ""

    def get_image_metadata(self, image_id: str = None) -> str:
        """
        特定の画像または全ての画像のメタデータを取得

        Args:
            image_id: 画像ID（Noneの場合は全ての画像のメタデータを返す）

        Returns:
            画像メタデータのMarkdown形式のテキスト
        """
        metadata_content = self.file_tool._run("read", self.images_metadata_file)

        if not image_id:
            return metadata_content

        # 特定の画像のメタデータを抽出
        sections = metadata_content.split("---")
        for section in sections:
            if section.strip() and f"## {image_id}" in section:
                return section.strip()

        return "指定された画像が見つかりません。"

    def search_images(self, query: str, k: int = 5) -> str:
        """
        キーワードで画像を検索

        Args:
            query: 検索クエリ
            k: 取得する結果の最大数

        Returns:
            検索結果のMarkdown形式のテキスト
        """
        try:
            # ベクトルメモリで画像を検索
            filter_dict = {"type": "image"}
            search_results = self.vector_memory.search(query, k, filter_dict)

            if not search_results:
                return "検索条件に一致する画像は見つかりませんでした。"

            result = "# 画像検索結果\n\n"

            for i, item in enumerate(search_results):
                metadata = item["metadata"]
                image_id = metadata.get("image_id", "不明")
                title = metadata.get("title", "無題")
                description = metadata.get("description", "")
                tags = metadata.get("tags", "")

                result += f"## {i+1}. {title} ({image_id})\n\n"
                if description:
                    result += f"- **説明**: {description}\n"
                if tags:
                    result += f"- **タグ**: {tags}\n"
                result += f"- **コマンド**: /image show {image_id}\n\n"

            return result
        except Exception as e:
            print(f"画像検索に失敗しました: {str(e)}")
            return "画像検索中にエラーが発生しました。"


class FeedbackAnalyzer:
    """ユーザーの反応からフィードバックを分析するクラス"""

    def __init__(self, llm):
        """
        初期化

        Args:
            llm: 言語モデル
        """
        self.llm = llm

    def detect_sentiment(self, text: str) -> Dict[str, Any]:
        """
        テキストから感情を検出

        Args:
            text: 分析するテキスト

        Returns:
            感情分析結果の辞書
        """
        prompt = PromptTemplate(
            input_variables=["text"],
            template="""
ユーザーの以下のテキストから感情を分析してください。JSON形式で以下の情報を返してください：
- sentiment: 全体的な感情（positive, neutral, negative）
- satisfaction_score: 満足度スコア（1-5、1が最も不満、5が最も満足）
- emotion: 主要な感情（happy, grateful, confused, frustrated, angry, disappointed, neutral など）
- feedback_type: フィードバックのタイプ（praise, criticism, question, suggestion, neutral）
- implicit_feedback: 暗黙的なフィードバックの内容（ユーザーが明示的に述べていないが、文脈から読み取れるフィードバック）

テキスト:
{text}

JSON形式の分析結果:
""",
        )

        chain = LLMChain(llm=self.llm, prompt=prompt)
        result = chain.run(text=text)

        try:
            # 結果をJSON形式で解析
            # 結果がJSON形式でない場合に備えて、前後の余分なテキストを削除
            json_str = re.search(r"\{.*\}", result, re.DOTALL)
            if json_str:
                return json.loads(json_str.group(0))
            else:
                return json.loads(result)
        except:
            # JSON解析に失敗した場合はデフォルト値を返す
            return {
                "sentiment": "neutral",
                "satisfaction_score": 3,
                "emotion": "neutral",
                "feedback_type": "neutral",
                "implicit_feedback": "特に検出されませんでした",
            }

    def extract_feedback(
        self, conversation_history: List[Dict[str, str]], current_response: str
    ) -> Dict[str, Any]:
        """
        会話履歴と現在の応答からフィードバックを抽出

        Args:
            conversation_history: 会話履歴のリスト
            current_response: 現在のユーザー応答

        Returns:
            フィードバック情報の辞書
        """
        # 会話履歴をテキスト形式に変換
        history_text = ""
        for i, message in enumerate(
            conversation_history[-3:]
        ):  # 直近3つのメッセージを使用
            role = "ユーザー" if message["role"] == "user" else "AI"
            history_text += f"{role}: {message['content']}\n"

        prompt = PromptTemplate(
            input_variables=["history", "current"],
            template="""
以下の会話履歴と現在のユーザー応答から、AIの前回の応答に対するフィードバックを抽出してください。
ユーザーが明示的にフィードバックを述べていなくても、暗黙的なフィードバックを検出してください。

会話履歴:
{history}

現在のユーザー応答:
{current}

JSON形式で以下の情報を返してください：
- has_feedback: フィードバックが含まれているか（true/false）
- feedback_type: フィードバックのタイプ（explicit:明示的, implicit:暗黙的, none:なし）
- sentiment: 感情（positive, neutral, negative）
- satisfaction_score: 満足度スコア（1-5、1が最も不満、5が最も満足）
- feedback_content: 検出されたフィードバックの内容
- requires_improvement: 応答の改善が必要か（true/false）

JSON形式の分析結果:
""",
        )

        chain = LLMChain(llm=self.llm, prompt=prompt)
        result = chain.run(history=history_text, current=current_response)

        try:
            # 結果をJSON形式で解析
            json_str = re.search(r"\{.*\}", result, re.DOTALL)
            if json_str:
                return json.loads(json_str.group(0))
            else:
                return json.loads(result)
        except:
            # JSON解析に失敗した場合はデフォルト値を返す
            return {
                "has_feedback": False,
                "feedback_type": "none",
                "sentiment": "neutral",
                "satisfaction_score": 3,
                "feedback_content": "",
                "requires_improvement": False,
            }

    def generate_improved_response(
        self, original_response: str, feedback: Dict[str, Any]
    ) -> str:
        """
        フィードバックに基づいて改善された応答を生成

        Args:
            original_response: 元の応答
            feedback: フィードバック情報

        Returns:
            改善された応答
        """
        if not feedback["requires_improvement"]:
            return original_response

        prompt = PromptTemplate(
            input_variables=["original_response", "feedback_content", "sentiment"],
            template="""
あなたは高品質なAIアシスタントです。以下の元の応答に対するユーザーフィードバックを分析し、改善された応答を生成してください。

# 元の応答
{original_response}

# ユーザーフィードバック
{feedback_content}

# ユーザーの感情
{sentiment}

# 改善された応答
""",
        )

        chain = LLMChain(llm=self.llm, prompt=prompt)
        improved_response = chain.run(
            original_response=original_response,
            feedback_content=feedback["feedback_content"],
            sentiment=feedback["sentiment"],
        )

        return improved_response


class PersonalAI:
    """パーソナライズされたAIアシスタント - Ollama Llama 3.3版"""

    def __init__(self, user_id: str, ollama_model: str = "llama3.3"):
        """
        初期化

        Args:
            user_id: ユーザーの一意識別子
            ollama_model: 使用するOllamaモデル名
        """
        self.user_id = user_id
        self.ollama_model = ollama_model

        # Ollamaが利用可能か確認
        self._check_ollama()

        # ツールの初期化
        self.file_tool = MarkdownFileTool()

        # ベクトルメモリの初期化
        self.vector_memory = VectorMemory(user_id, ollama_model)

        # メモリの初期化
        self.memory = MarkdownMemory(
            user_id=user_id,
            file_tool=self.file_tool,
            vector_memory=self.vector_memory,
            ollama_model=ollama_model,
        )

        # 会話履歴
        self.conversation_history = []
        self.current_conversation_id = None
        self.last_response = None

        # LLMの設定
        self.llm = OllamaLLM(
            model=ollama_model, temperature=0.7, callbacks=[StdOutCallbackHandler()]
        )

        # フィードバック分析器の初期化
        self.feedback_analyzer = FeedbackAnalyzer(
            OllamaLLM(model=ollama_model, temperature=0.2)
        )

        # コマンド処理ハンドラー
        self.command_handlers = {
            "/help": self._handle_help,
            "/remember": self._handle_remember,
            "/forget": self._handle_forget,
            "/profile": self._handle_profile,
            "/settings": self._handle_settings,
            "/set": self._handle_set,
            "/history": self._handle_history,
            "/clear": self._handle_clear,
            "/search": self._handle_search,
            "/feedback": self._handle_feedback,
            "/satisfaction": self._handle_satisfaction,
            "/vector": self._handle_vector,
            "/export": self._handle_export,
            "/import": self._handle_import,
            "/image": self._handle_image,  # 画像関連コマンドを追加
        }

        # 記憶する情報の候補
        self.memory_candidate = None

    def _check_ollama(self):
        """Ollamaが利用可能か確認し、必要に応じてモデルをプルする"""
        try:
            # Ollamaコマンドが利用可能か確認
            subprocess.run(["ollama", "list"], check=True, capture_output=True)

            # モデルが存在するか確認
            result = subprocess.run(
                ["ollama", "list"], check=True, capture_output=True, text=True
            )
            if self.ollama_model not in result.stdout:
                print(f"モデル {self.ollama_model} をプルしています...")
                subprocess.run(["ollama", "pull", self.ollama_model], check=True)

        except subprocess.CalledProcessError:
            print(
                "Ollamaが見つかりません。インストールしてください: https://ollama.com/download"
            )
            raise
        except Exception as e:
            print(f"Ollamaの確認中にエラーが発生しました: {str(e)}")
            raise

    def save_profile_info(self, key: str, value: str) -> str:
        """ユーザープロファイル情報を保存または更新"""
        self.memory.save_profile_info(key, value)
        return f"プロファイル情報 '{key}' を更新しました"

    def save_preference(self, category: str, key: str, value: str) -> None:
        """ユーザー設定を保存または更新"""
        self.memory.save_preference(category, key, value)

    def save_conversation(
        self, human_input: str, ai_output: str, conversation_id: str = None
    ) -> str:
        """
        会話をMarkdownファイルに保存

        Args:
            human_input: ユーザーの入力
            ai_output: AIの応答
            conversation_id: 会話ID（Noneの場合は新しいIDを生成）

        Returns:
            conversation_id: 会話ID
        """
        return self.memory.save_conversation(human_input, ai_output, conversation_id)

    def save_feedback(
        self,
        conversation_id: str,
        feedback_type: str,
        feedback_content: str,
        original_response: str = None,
        improved_response: str = None,
    ) -> None:
        """
        ユーザーフィードバックを保存

        Args:
            conversation_id: フィードバック対象の会話ID
            feedback_type: フィードバックの種類（明示的/暗黙的/感情/提案など）
            feedback_content: フィードバックの内容
            original_response: 元の応答（オプション）
            improved_response: 改善された応答（オプション）
        """
        self.memory.save_feedback(
            conversation_id,
            feedback_type,
            feedback_content,
            original_response,
            improved_response,
        )

    def save_satisfaction(
        self, conversation_id: str, satisfaction_level: int, comment: str = None
    ) -> None:
        """
        ユーザー満足度を保存

        Args:
            conversation_id: 満足度評価対象の会話ID
            satisfaction_level: 満足度レベル（1-5）
            comment: コメント（オプション）
        """
        self.memory.save_satisfaction(conversation_id, satisfaction_level, comment)

    def get_recent_conversations(self, n: int = 5) -> str:
        """最近のn件の会話を取得"""
        return self.memory.get_recent_conversations(n)

    def get_profile(self) -> str:
        """ユーザープロファイルを取得"""
        return self.memory.get_profile()

    def get_preferences(self) -> str:
        """ユーザー設定を取得"""
        return self.memory.get_preferences()

    def get_feedback_patterns(self) -> str:
        """フィードバックパターンを取得"""
        return self.memory.get_feedback_patterns()

    def get_satisfaction_trends(self) -> str:
        """満足度傾向を取得"""
        return self.memory.get_satisfaction_trends()

    def process_command(self, command: str) -> Optional[str]:
        """
        コマンドを処理する

        Args:
            command: ユーザーが入力したコマンド

        Returns:
            コマンドの実行結果、コマンドでない場合はNone
        """
        if not command.startswith("/"):
            return None

        parts = command.split()
        cmd = parts[0].lower()
        args = parts[1:] if len(parts) > 1 else []

        if cmd in self.command_handlers:
            return self.command_handlers[cmd](args)

        return f"不明なコマンド: {cmd}\n/help で利用可能なコマンドを確認できます。"

    def _handle_help(self, args: List[str]) -> str:
        """ヘルプコマンドを処理"""
        return """利用可能なコマンド:
/help - このヘルプを表示
/remember <キー> <値> - 情報を記憶する (例: /remember 誕生日 4月1日)
/forget <キー> - 記憶した情報を忘れる
/profile - 記憶している情報を表示
/settings [カテゴリ] - 設定を表示
/set <カテゴリ> <キー> <値> - 設定を変更
/history [数] - 過去の会話履歴を表示
/clear - 現在の会話履歴をクリア
/search <クエリ> - 関連する記憶を検索
/feedback <タイプ> <内容> - フィードバックを記録
/satisfaction <レベル> [コメント] - 満足度を記録 (レベル: 1-5)
/vector clear - ベクトル記憶をクリア
/export <ファイル名> - 設定をエクスポート
/import <ファイル名> - 設定をインポート
/image <サブコマンド> - 画像関連コマンド
  - /image save <画像パス> <タイトル> [説明] [タグ] - 画像を保存
  - /image list - 保存した画像の一覧を表示
  - /image show <画像ID> - 指定した画像を表示
  - /image search <キーワード> - 画像を検索
"""

    def _handle_remember(self, args: List[str]) -> str:
        """記憶コマンドを処理"""
        if len(args) < 2:
            return "使用法: /remember <キー> <値>"

        key = args[0]
        value = " ".join(args[1:])

        self.save_profile_info(key, value)
        return f"「{key}: {value}」を記憶しました。"

    def _handle_forget(self, args: List[str]) -> str:
        """忘却コマンドを処理"""
        if len(args) < 1:
            return "使用法: /forget <キー>"

        key = args[0]
        profile_content = self.get_profile()

        # 既存のプロファイルから該当キーを削除
        lines = profile_content.split("\n")
        found = False

        for i, line in enumerate(lines):
            if line.startswith(f"- **{key}**:"):
                lines.pop(i)
                found = True
                break

        if found:
            self.file_tool._run("write", self.memory.profile_file, "\n".join(lines))
            return f"「{key}」を忘れました。"
        else:
            return f"「{key}」は記憶されていません。"

    def _handle_profile(self, args: List[str]) -> str:
        """プロファイル表示コマンドを処理"""
        profile = self.get_profile()

        if not profile or profile == "# ユーザープロファイル\n\n":
            return "プロファイル情報はまだ記録されていません。"

        return profile

    def _handle_settings(self, args: List[str]) -> str:
        """設定表示コマンドを処理"""
        preferences = self.get_preferences()

        if not preferences or preferences == "# ユーザー設定\n\n":
            return "設定情報はまだ記録されていません。"

        if args and len(args) > 0:
            # カテゴリが指定された場合はそのカテゴリのみ表示
            category = args[0]
            lines = preferences.split("\n")
            result = ["# 設定"]
            in_category = False

            for line in lines:
                if line.startswith(f"## {category}"):
                    in_category = True
                    result.append(line)
                elif in_category and line.startswith("## "):
                    in_category = False
                elif in_category:
                    result.append(line)

            if len(result) == 1:
                return f"カテゴリ '{category}' の設定は見つかりませんでした。"

            return "\n".join(result)

        return preferences

    def _handle_set(self, args: List[str]) -> str:
        """設定変更コマンドを処理"""
        if len(args) < 3:
            return "使用法: /set <カテゴリ> <キー> <値>"

        category = args[0]
        key = args[1]
        value = " ".join(args[2:])

        self.save_preference(category, key, value)
        return f"設定 '{category}.{key}' を '{value}' に更新しました。"

    def _handle_history(self, args: List[str]) -> str:
        """履歴表示コマンドを処理"""
        n = 5  # デフォルト

        if args and args[0].isdigit():
            n = int(args[0])

        history = self.get_recent_conversations(n)

        if not history:
            return "会話履歴はまだありません。"

        return history

    def _handle_clear(self, args: List[str]) -> str:
        """履歴クリアコマンドを処理"""
        self.conversation_history = []
        self.current_conversation_id = None
        return "現在の会話履歴をクリアしました。"

    def detect_memorizable_info(self, text: str) -> Optional[Dict[str, str]]:
        """
        テキストから記憶すべき情報を検出する

        Args:
            text: ユーザーの入力テキスト

        Returns:
            記憶すべき情報（キーと値のペア）またはNone
        """
        # 情報抽出用のプロンプト作成
        prompt = PromptTemplate(
            input_variables=["text"],
            template="""
ユーザーの発言から記憶すべき重要な情報を抽出してください。
ユーザーが「〜は〜だ」「〜を〜に置いた」など、事実や好み、位置情報などを述べている場合は、それを記憶すべき情報として抽出してください。
単なる質問や会話の一部だけの場合は何も抽出しないでください。

## ユーザーの発言
{text}

JSONで以下の形式で応答してください：
{{"should_memorize": true/false, "key": "抽出したキー", "value": "抽出した値"}}

例：
「私の誕生日は7月7日だよ」→ {{"should_memorize": true, "key": "誕生日", "value": "7月7日"}}
「HHKBはデスクの左側に置いてある」→ {{"should_memorize": true, "key": "HHKB", "value": "デスクの左側に置いてある"}}
「今日の天気はどう？」→ {{"should_memorize": false, "key": "", "value": ""}}
""",
        )

        # LLMチェーンで情報を抽出
        chain = LLMChain(llm=self.llm, prompt=prompt)
        result = chain.run(text=text)

        try:
            # 結果をJSON形式で解析
            json_str = re.search(r"\{.*\}", result, re.DOTALL)
            if json_str:
                data = json.loads(json_str.group(0))
                if data.get("should_memorize", False):
                    return {"key": data.get("key", ""), "value": data.get("value", "")}
            return None
        except:
            return None

    def _handle_search(self, args: List[str]) -> str:
        """記憶検索コマンドを処理"""
        if not args:
            return "使用法: /search <検索クエリ>"

        query = " ".join(args)
        related_memories = self.memory.search_related_memories(query)

        if not related_memories:
            return "関連する記憶は見つかりませんでした。"

        return related_memories

    def _handle_feedback(self, args: List[str]) -> str:
        """フィードバック記録コマンドを処理"""
        if len(args) < 2:
            return "使用法: /feedback <タイプ> <内容>\nタイプ例: praise, criticism, suggestion, question"

        feedback_type = args[0]
        feedback_content = " ".join(args[1:])

        if self.current_conversation_id:
            self.save_feedback(
                self.current_conversation_id, feedback_type, feedback_content
            )
            return f"フィードバック '{feedback_type}' を記録しました。"
        else:
            return "フィードバックを記録するには先に会話が必要です。"

    def _handle_satisfaction(self, args: List[str]) -> str:
        """満足度記録コマンドを処理"""
        if not args:
            return "使用法: /satisfaction <レベル> [コメント]\nレベル: 1-5 (1: とても不満、5: とても満足)"

        try:
            satisfaction_level = int(args[0])
            if satisfaction_level < 1 or satisfaction_level > 5:
                return "満足度レベルは1から5の整数で指定してください。"

            comment = " ".join(args[1:]) if len(args) > 1 else None

            if self.current_conversation_id:
                self.save_satisfaction(
                    self.current_conversation_id, satisfaction_level, comment
                )
                return f"満足度 {satisfaction_level}/5 を記録しました。"
            else:
                return "満足度を記録するには先に会話が必要です。"
        except ValueError:
            return "満足度レベルは整数で指定してください。"

    def _handle_vector(self, args: List[str]) -> str:
        """ベクトルメモリ管理コマンドを処理"""
        if not args:
            return "使用法: /vector <サブコマンド>\nサブコマンド: clear"

        subcommand = args[0].lower()

        if subcommand == "clear":
            self.vector_memory.clear()
            return "ベクトルメモリをクリアしました。"
        else:
            return f"不明なサブコマンド: {subcommand}"

    def _handle_export(self, args: List[str]) -> str:
        """設定エクスポートコマンドを処理"""
        if not args:
            return "使用法: /export <ファイル名>"

        file_name = args[0]
        if not file_name.endswith(".json"):
            file_name += ".json"

        # エクスポートするデータ
        export_data = {
            "profile": self.get_profile(),
            "preferences": self.get_preferences(),
        }

        try:
            with open(file_name, "w", encoding="utf-8") as f:
                json.dump(export_data, f, ensure_ascii=False, indent=2)
            return f"設定を '{file_name}' にエクスポートしました。"
        except Exception as e:
            return f"エクスポート中にエラーが発生しました: {str(e)}"

    def _handle_import(self, args: List[str]) -> str:
        """設定インポートコマンドを処理"""
        if not args:
            return "使用法: /import <ファイル名>"

        file_name = args[0]
        if not os.path.exists(file_name):
            return f"ファイル '{file_name}' が見つかりません。"

        try:
            with open(file_name, "r", encoding="utf-8") as f:
                import_data = json.load(f)

            # プロファイルをインポート
            if "profile" in import_data:
                profile_text = import_data["profile"]
                profile_lines = profile_text.split("\n")

                for line in profile_lines:
                    if line.startswith("- **") and "**: " in line:
                        key_value = line[line.index("**") + 2 :]
                        key = key_value[: key_value.index("**")]
                        value = key_value[key_value.index("**: ") + 4 :]
                        self.save_profile_info(key, value)

            # 設定をインポート
            if "preferences" in import_data:
                prefs_text = import_data["preferences"]
                current_category = None

                for line in prefs_text.split("\n"):
                    if line.startswith("## "):
                        current_category = line[3:].strip()
                    elif (
                        line.startswith("- **") and "**: " in line and current_category
                    ):
                        key_value = line[line.index("**") + 2 :]
                        key = key_value[: key_value.index("**")]
                        value = key_value[key_value.index("**: ") + 4 :]
                        self.save_preference(current_category, key, value)

            return "設定をインポートしました。"
        except Exception as e:
            return f"インポート中にエラーが発生しました: {str(e)}"

    def _handle_image(self, args: List[str]) -> str:
        """画像関連コマンドを処理"""
        if not args:
            return (
                "使用法: /image <サブコマンド>\nサブコマンド: save, list, show, search"
            )

        subcommand = args[0].lower()

        if subcommand == "save":
            if len(args) < 3:
                return (
                    "使用法: /image save <画像パス> <タイトル> [説明] [タグ1,タグ2,...]"
                )

            image_path = args[1]
            title = args[2]
            description = " ".join(args[3:]) if len(args) > 3 else ""
            tags = args[3:] if len(args) > 3 else None

            return self.memory.save_image(image_path, title, description, tags)
        elif subcommand == "list":
            return self.memory.get_image_metadata()
        elif subcommand == "show":
            if len(args) < 1:
                return "使用法: /image show <画像ID>"

            image_id = args[0]
            image_path = self.memory.get_image_path(image_id)
            if image_path:
                return f"画像 '{image_id}' を表示します。"
            else:
                return "指定された画像が見つかりません。"
        elif subcommand == "search":
            if len(args) < 1:
                return "使用法: /image search <キーワード>"

            query = " ".join(args)
            return self.memory.search_images(query)
        else:
            return f"不明なサブコマンド: {subcommand}"

    def generate_response(self, human_input: str) -> str:
        """
        ユーザーの入力に基づいてAIの応答を生成

        Args:
            human_input: ユーザーの入力

        Returns:
            AIの応答
        """
        # 記憶候補の確認応答処理
        if self.memory_candidate is not None:
            if human_input.lower() in ["y", "yes", "はい", "うん", "そう"]:
                key = self.memory_candidate["key"]
                value = self.memory_candidate["value"]
                self.save_profile_info(key, value)
                self.memory_candidate = None
                return f"「{key}: {value}」を記憶しました。"
            elif human_input.lower() in ["n", "no", "いいえ", "違う", "ちがう"]:
                self.memory_candidate = None
                return "わかりました。記憶しません。"

        # コマンド処理
        command_result = self.process_command(human_input)
        if command_result is not None:
            return command_result

        # 会話履歴を更新
        self.conversation_history.append({"role": "user", "content": human_input})

        # 保存されている情報を取得
        recent_conversations = self.get_recent_conversations(5)
        profile_info = self.get_profile()
        preferences = self.get_preferences()

        # 関連する記憶を検索
        related_memories = self.memory.search_related_memories(human_input)

        # 現在の日時情報を取得
        current_datetime = datetime.datetime.now().strftime("%Y年%m月%d日 %H時%M分")
        current_day_of_week = [
            "月曜日",
            "火曜日",
            "水曜日",
            "木曜日",
            "金曜日",
            "土曜日",
            "日曜日",
        ][datetime.datetime.now().weekday()]

        # 位置情報を取得（エラー処理を含む）
        location_info = "利用不可"
        try:
            g = geocoder.ip("me")
            if g.ok and g.city:
                location_info = f"{g.city}, {g.state}, {g.country}"
        except Exception:
            pass

        # プロンプト作成
        prompt = PromptTemplate(
            input_variables=[
                "history",
                "question",
                "profile",
                "preferences",
                "recent_conversations",
                "current_datetime",
                "day_of_week",
                "location",
                "related_memories",
            ],
            template="""
ユーザーの質問に対して、保存されている情報と会話履歴に基づいて役立つ応答を生成してください。
ユーザーの好みやプロファイル情報を考慮してください。
特に関連性の高い記憶を適切に活用してください。

## システム情報
現在日時: {current_datetime} ({day_of_week})
現在位置: {location}

## ユーザープロファイル
{profile}

## ユーザー設定
{preferences}

## 関連する記憶（質問に関連性の高い順）
{related_memories}

## 最近の会話
{recent_conversations}

## 現在の会話履歴
{history}

## 現在の質問
{question}

## 応答
""",
        )

        # 会話履歴をテキスト形式に変換
        history_text = ""
        for message in self.conversation_history[-5:]:  # 直近5つのメッセージを使用
            role = "ユーザー" if message["role"] == "user" else "AI"
            history_text += f"{role}: {message['content']}\n"

        # LLMチェーンを使用して応答を生成
        chain = LLMChain(llm=self.llm, prompt=prompt)
        response = chain.run(
            history=history_text,
            question=human_input,
            profile=profile_info,
            preferences=preferences,
            recent_conversations=recent_conversations,
            current_datetime=current_datetime,
            day_of_week=current_day_of_week,
            location=location_info,
            related_memories=related_memories,
        )

        # 記憶すべき情報があるか検出
        memorizable_info = self.detect_memorizable_info(human_input)

        # 会話IDを生成または更新
        if not self.current_conversation_id:
            timestamp = datetime.datetime.now().strftime("%Y-%m-%d_%H-%M-%S")
            self.current_conversation_id = f"conv_{timestamp}"

        # 会話を保存
        self.save_conversation(human_input, response, self.current_conversation_id)

        # 応答を会話履歴に追加
        self.conversation_history.append({"role": "AI", "content": response})
        self.last_response = response

        # 記憶すべき情報があれば候補として保存（次の入力でy/nを確認する）
        if memorizable_info:
            self.memory_candidate = memorizable_info
            return f"{response}\n\n「{memorizable_info['key']}: {memorizable_info['value']}」を記憶しておきますか？(y/n)"

        return response

    def save_image(
        self, image_path: str, title: str, description: str = "", tags: List[str] = None
    ) -> str:
        """
        画像を保存する

        Args:
            image_path: 元の画像ファイルのパス
            title: 画像のタイトル
            description: 画像の説明
            tags: 画像に関連するタグのリスト

        Returns:
            保存された画像のID
        """
        return self.memory.save_image(image_path, title, description, tags)

    def get_image_path(self, image_id: str) -> str:
        """
        画像IDから画像ファイルのパスを取得

        Args:
            image_id: 画像ID

        Returns:
            画像ファイルのパス
        """
        return self.memory.get_image_path(image_id)

    def get_image_metadata(self, image_id: str = None) -> str:
        """
        画像のメタデータを取得

        Args:
            image_id: 画像ID（Noneの場合は全ての画像のメタデータを返す）

        Returns:
            画像メタデータのテキスト
        """
        return self.memory.get_image_metadata(image_id)

    def search_images(self, query: str, k: int = 5) -> str:
        """
        画像を検索

        Args:
            query: 検索クエリ
            k: 取得する結果の最大数

        Returns:
            検索結果のテキスト
        """
        return self.memory.search_images(query, k)


if __name__ == "__main__":
    import argparse

    # コマンドライン引数の処理
    parser = argparse.ArgumentParser(description="パーソナライズAIアシスタント")
    parser.add_argument(
        "--user_id", type=str, default="default_user", help="ユーザーID"
    )
    parser.add_argument(
        "--model", type=str, default="llama3.3", help="使用するOllamaモデル"
    )
    args = parser.parse_args()

    # リッチコンソールの初期化
    console = Console()

    # PersonalAIの初期化
    ai = PersonalAI(user_id=args.user_id, ollama_model=args.model)

    # スプラッシュ画面表示
    console.print(
        Panel.fit(
            "[bold cyan]パーソナルAIアシスタント[/bold cyan] - [yellow]{args.user_id}[/yellow]",
            subtitle="[dim]llama3.3[/dim]",
        )
    )

    console.print(
        "\n[dim]コマンド一覧を表示するには [bold]/help[/bold] と入力してください。[/dim]"
    )
    console.print(
        "[dim]終了するには [bold]exit[/bold] または [bold]quit[/bold] と入力してください。[/dim]\n"
    )

    # すべての出力を抑制
    stderr_buffer = io.StringIO()
    stdout_buffer = io.StringIO()

    # 対話ループ
    while True:
        try:
            # ユーザー入力の取得
            with redirect_stderr(stderr_buffer):
                user_input = Prompt.ask("\n[bold green]あなた[/bold green]")

            # 終了コマンドのチェック
            if user_input.lower() in ["exit", "quit", "終了"]:
                console.print("[bold yellow]さようなら！[/bold yellow]")
                break

            # AIの応答を生成（コマンド処理も含む）
            console.print("[dim]処理中...[/dim]")

            # 警告出力を抑制
            with redirect_stderr(stderr_buffer):
                with redirect_stdout(stdout_buffer):
                    response = ai.generate_response(user_input)

            # 応答を表示
            if user_input.startswith("/") and not ai.memory_candidate:
                # コマンド結果の表示
                if response.startswith("# "):
                    # Markdown表示
                    console.print(Markdown(response))
                else:
                    # 通常テキスト表示
                    console.print(Panel(response, title="結果", border_style="blue"))
            else:
                # 通常の会話応答
                if ai.memory_candidate:
                    # 記憶確認を含む応答
                    main_response, confirmation = response.rsplit("\n\n", 1)
                    console.print(f"\n[bold purple]AI[/bold purple]: {main_response}")
                    console.print(Panel(confirmation, border_style="yellow"))
                else:
                    # 通常の応答
                    console.print(f"\n[bold purple]AI[/bold purple]: {response}")

        except KeyboardInterrupt:
            # エラー出力をクリア
            stderr_buffer.truncate(0)
            stderr_buffer.seek(0)
            stdout_buffer.truncate(0)
            stdout_buffer.seek(0)

            console.print("\n\n[bold red]プログラムを終了します...[/bold red]")
            sys.exit(0)
        except Exception as e:
            console.print(f"\n[bold red]エラーが発生しました:[/bold red] {str(e)}")
