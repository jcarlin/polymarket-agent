import logging
import os
from contextlib import asynccontextmanager

import uvicorn
from fastapi import FastAPI, HTTPException
from pydantic import BaseModel

from polymarket_client import PolymarketClient

logger = logging.getLogger("sidecar")

SIDECAR_PORT = int(os.getenv("SIDECAR_PORT", "9090"))
TRADING_MODE = os.getenv("TRADING_MODE", "paper")

# Global client instance — initialized at startup
polymarket = PolymarketClient()


class HealthResponse(BaseModel):
    status: str
    version: str
    trading_mode: str


class OrderRequest(BaseModel):
    token_id: str
    price: float
    size: float
    side: str


class OrderResponse(BaseModel):
    order_id: str
    status: str
    price: float
    size: float


@asynccontextmanager
async def lifespan(app: FastAPI):
    logger.info("Sidecar starting on port %d in %s mode", SIDECAR_PORT, TRADING_MODE)
    if TRADING_MODE == "live":
        if polymarket.initialize():
            logger.info("Polymarket client ready for live trading")
        else:
            logger.warning("Polymarket client failed to initialize — /order will return 503")
    else:
        logger.info("Paper mode — Polymarket client not initialized")
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


@app.post("/order", response_model=OrderResponse)
async def place_order(req: OrderRequest):
    if not polymarket.is_initialized:
        raise HTTPException(
            status_code=503,
            detail="Polymarket client not initialized (paper mode or missing private key)",
        )

    try:
        result = polymarket.place_order(
            token_id=req.token_id,
            price=req.price,
            size=req.size,
            side=req.side,
        )
        return OrderResponse(
            order_id=result["order_id"],
            status=result["status"],
            price=req.price,
            size=req.size,
        )
    except Exception as e:
        logger.error("Order placement failed: %s", e)
        raise HTTPException(status_code=500, detail=str(e))


if __name__ == "__main__":
    logging.basicConfig(
        level=logging.INFO,
        format="%(asctime)s [%(name)s] %(levelname)s: %(message)s",
    )
    uvicorn.run(app, host="0.0.0.0", port=SIDECAR_PORT, log_level="info")
