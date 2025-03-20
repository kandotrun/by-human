import asyncio
import os
from dolphin_mcp import run_interaction

async def main():
    user_query = "こんにちは。"
    config_path = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "..", "mcp_config.json")
    result = await run_interaction(
        user_query=user_query,
        model_name="llama3.3",
        config_path=config_path
    )
    print(result)

if __name__ == "__main__":
    asyncio.run(main())