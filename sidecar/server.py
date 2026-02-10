import logging
import os
from contextlib import asynccontextmanager

import uvicorn
from fastapi import FastAPI
from pydantic import BaseModel

logger = logging.getLogger("sidecar")

SIDECAR_PORT = int(os.getenv("SIDECAR_PORT", "9090"))
TRADING_MODE = os.getenv("TRADING_MODE", "paper")


class HealthResponse(BaseModel):
    status: str
    version: str
    trading_mode: str


@asynccontextmanager
async def lifespan(app: FastAPI):
    logger.info("Sidecar starting on port %d in %s mode", SIDECAR_PORT, TRADING_MODE)
    yield
    logger.info("Sidecar shutting down")


app = FastAPI(title="Polymarket Sidecar", version="0.1.0", lifespan=lifespan)


@app.get("/health", response_model=HealthResponse)
async def health():
    return HealthResponse(
        status="ok",
        version="0.1.0",
        trading_mode=TRADING_MODE,
    )


if __name__ == "__main__":
    logging.basicConfig(
        level=logging.INFO,
        format="%(asctime)s [%(name)s] %(levelname)s: %(message)s",
    )
    uvicorn.run(app, host="0.0.0.0", port=SIDECAR_PORT, log_level="info")
