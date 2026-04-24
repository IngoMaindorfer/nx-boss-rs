import argparse
import pathlib

import uvicorn

from nx_boss.app import create_app
from nx_boss.config import Config


def main() -> None:
    parser = argparse.ArgumentParser(description="PaperStream NX Manager compatible server")
    parser.add_argument("--host", type=str, default="127.0.0.1")
    parser.add_argument("--port", type=int, default=10447)
    parser.add_argument("--config", "-c", type=str, required=True)
    args = parser.parse_args()

    config = Config.load(pathlib.Path(args.config))
    app = create_app(config)
    uvicorn.run(app, host=args.host, port=args.port)


if __name__ == "__main__":
    main()
