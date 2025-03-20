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
from typing import List, Dict, Any, Optional, Tuple

from langchain_ollama import OllamaLLM
from langchain.chains import LLMChain
from langchain.prompts import PromptTemplate
from langchain.tools import BaseTool
from langchain.callbacks import StdOutCallbackHandler

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

class MarkdownMemory:
    """Markdownファイルを使用して会話履歴と個人情報を保存するカスタムメモリクラス"""
    
    def __init__(self, user_id: str, memory_dir: str = "memories", file_tool: MarkdownFileTool = None):
        """
        初期化
        
        Args:
            user_id: ユーザーの一意識別子
            memory_dir: 記憶を保存するディレクトリ
            file_tool: ファイル操作ツール
        """
        self.user_id = user_id
        self.memory_dir = memory_dir
        self.conversation_file = f"{memory_dir}/{user_id}_conversations.md"
        self.profile_file = f"{memory_dir}/{user_id}_profile.md"
        self.preferences_file = f"{memory_dir}/{user_id}_preferences.md"
        self.feedback_file = f"{memory_dir}/{user_id}_feedback.md"
        self.satisfaction_file = f"{memory_dir}/{user_id}_satisfaction.md"
        
        # ファイル操作ツール
        self.file_tool = file_tool if file_tool else MarkdownFileTool()
        
        # ディレクトリが存在しない場合は作成
        os.makedirs(memory_dir, exist_ok=True)
        
        # 各ファイルが存在しない場合は作成
        for file_path, header in [
            (self.conversation_file, "# 会話履歴\n\n"),
            (self.profile_file, "# ユーザープロファイル\n\n"),
            (self.preferences_file, "# ユーザー設定\n\n"),
            (self.feedback_file, "# フィードバック履歴\n\n"),
            (self.satisfaction_file, "# 満足度履歴\n\n")
        ]:
            if not os.path.exists(file_path):
                self.file_tool._run("write", file_path, header)
    
    def save_conversation(self, human_input: str, ai_output: str, conversation_id: str = None) -> str:
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
        
        self.file_tool._run("append", self.conversation_file, content)
            
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
                lines = ["# ユーザー設定\n", f"## {category}\n", f"- **{key}**: {value}"]
        elif not key_exists:
            # カテゴリは存在するがキーが存在しない場合
            # カテゴリの後に挿入
            insert_index = category_index + 1
            while insert_index < len(lines) and not lines[insert_index].startswith("##"):
                insert_index += 1
            lines.insert(insert_index, f"- **{key}**: {value}")
        
        # 更新された内容を書き込む
        self.file_tool._run("write", self.preferences_file, "\n".join(lines))
    
    def save_feedback(self, conversation_id: str, feedback_type: str, feedback_content: str, 
                     original_response: str = None, improved_response: str = None) -> None:
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
    
    def save_satisfaction(self, conversation_id: str, satisfaction_level: int, comment: str = None) -> None:
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
        content = self.file_tool._run("read", self.conversation_file)
        
        if not content:
            return ""
        
        # 会話を分割（"---"で区切られている）
        conversations = content.split("---")
        
        # 最新のn件を取得（最後の空要素を除く）
        recent_conversations = conversations[-n-1:-1] if len(conversations) > 1 else conversations
        
        return "---".join(recent_conversations)
    
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
"""
        )
        
        chain = LLMChain(llm=self.llm, prompt=prompt)
        result = chain.run(text=text)
        
        try:
            # 結果をJSON形式で解析
            # 結果がJSON形式でない場合に備えて、前後の余分なテキストを削除
            json_str = re.search(r'\{.*\}', result, re.DOTALL)
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
                "implicit_feedback": "特に検出されませんでした"
            }
    
    def extract_feedback(self, conversation_history: List[Dict[str, str]], 
                        current_response: str) -> Dict[str, Any]:
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
        for i, message in enumerate(conversation_history[-3:]):  # 直近3つのメッセージを使用
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
"""
        )
        
        chain = LLMChain(llm=self.llm, prompt=prompt)
        result = chain.run(history=history_text, current=current_response)
        
        try:
            # 結果をJSON形式で解析
            json_str = re.search(r'\{.*\}', result, re.DOTALL)
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
                "requires_improvement": False
            }
    
    def generate_improved_response(self, original_response: str, 
                                 feedback: Dict[str, Any]) -> str:
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
"""
        )
        
        chain = LLMChain(llm=self.llm, prompt=prompt)
        improved_response = chain.run(
            original_response=original_response,
            feedback_content=feedback["feedback_content"],
            sentiment=feedback["sentiment"]
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
        
        # メモリの初期化
        self.memory = MarkdownMemory(user_id, file_tool=self.file_tool)
        
        # 会話履歴
        self.conversation_history = []
        self.current_conversation_id = None
        self.last_response = None
        
        # LLMの設定
        self.llm = OllamaLLM(
            model=ollama_model,
            temperature=0.7,
            callbacks=[StdOutCallbackHandler()]
        )
        
        # フィードバック分析器の初期化
        self.feedback_analyzer = FeedbackAnalyzer(
            OllamaLLM(model=ollama_model, temperature=0.2)
        )
    
    def _check_ollama(self):
        """Ollamaが利用可能か確認し、必要に応じてモデルをプルする"""
        try:
            # Ollamaコマンドが利用可能か確認
            subprocess.run(["ollama", "list"], check=True, capture_output=True)
            
            # モデルが存在するか確認
            result = subprocess.run(["ollama", "list"], check=True, capture_output=True, text=True)
            if self.ollama_model not in result.stdout:
                print(f"モデル {self.ollama_model} をプルしています...")
                subprocess.run(["ollama", "pull", self.ollama_model], check=True)
                
        except subprocess.CalledProcessError:
            print("Ollamaが見つかりません。インストールしてください: https://ollama.com/download")
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
    
    def save_conversation(self, human_input: str, ai_output: str, conversation_id: str = None) -> str:
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
    
    def save_feedback(self, conversation_id: str, feedback_type: str, feedback_content: str, 
                     original_response: str = None, improved_response: str = None) -> None:
        """
        ユーザーフィードバックを保存
        
        Args:
            conversation_id: フィードバック対象の会話ID
            feedback_type: フィードバックの種類（明示的/暗黙的/感情/提案など）
            feedback_content: フィードバックの内容
            original_response: 元の応答（オプション）
            improved_response: 改善された応答（オプション）
        """
        self.memory.save_feedback(conversation_id, feedback_type, feedback_content, original_response, improved_response)
    
    def save_satisfaction(self, conversation_id: str, satisfaction_level: int, comment: str = None) -> None:
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

    def generate_response(self, human_input: str) -> str:
        """
        ユーザーの入力に基づいてAIの応答を生成
        
        Args:
            human_input: ユーザーの入力
            
        Returns:
            AIの応答
        """
        # 会話履歴を更新
        self.conversation_history.append({"role": "user", "content": human_input})
        
        # 保存されている情報を取得
        recent_conversations = self.get_recent_conversations(5)
        profile_info = self.get_profile()
        preferences = self.get_preferences()
        
        # プロンプト作成
        prompt = PromptTemplate(
            input_variables=["history", "question", "profile", "preferences", "recent_conversations"],
            template="""
ユーザーの質問に対して、保存されている情報と会話履歴に基づいて役立つ応答を生成してください。
ユーザーの好みやプロファイル情報を考慮してください。

## ユーザープロファイル
{profile}

## ユーザー設定
{preferences}

## 最近の会話
{recent_conversations}

## 現在の会話履歴
{history}

## 現在の質問
{question}

## 応答
"""
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
            recent_conversations=recent_conversations
        )
        
        # 会話IDを生成または更新
        if not self.current_conversation_id:
            timestamp = datetime.datetime.now().strftime("%Y-%m-%d_%H-%M-%S")
            self.current_conversation_id = f"conv_{timestamp}"
        
        # 会話を保存
        self.save_conversation(human_input, response, self.current_conversation_id)
        
        # 応答を会話履歴に追加
        self.conversation_history.append({"role": "AI", "content": response})
        self.last_response = response
        
        return response

if __name__ == "__main__":
    import argparse
    
    # コマンドライン引数の処理
    parser = argparse.ArgumentParser(description="パーソナライズAIアシスタント")
    parser.add_argument("--user_id", type=str, default="default_user", help="ユーザーID")
    parser.add_argument("--model", type=str, default="llama3.3", help="使用するOllamaモデル")
    args = parser.parse_args()
    
    # PersonalAIの初期化
    ai = PersonalAI(user_id=args.user_id, ollama_model=args.model)
    
    print(f"=== パーソナルAIアシスタント - ユーザー: {args.user_id} ===")
    print("終了するには 'exit' または 'quit' と入力してください。")
    
    # 対話ループ
    while True:
        try:
            # ユーザー入力の取得
            user_input = input("\nあなた: ")
            
            # 終了コマンドのチェック
            if user_input.lower() in ["exit", "quit", "終了"]:
                print("さようなら！")
                break
            
            # AIの応答を生成
            response = ai.generate_response(user_input)
            
            # 応答を表示
            print(f"\nAI: {response}")
            
        except KeyboardInterrupt:
            print("\n\nプログラムを終了します...")
            break
        except Exception as e:
            print(f"\nエラーが発生しました: {str(e)}")