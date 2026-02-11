"""Polymarket CLOB client wrapper for the Python sidecar.

Handles order signing and placement via py-clob-client.
"""

import logging
import os
from typing import Any, Optional

logger = logging.getLogger("sidecar.polymarket_client")

CLOB_HOST = "https://clob.polymarket.com"
CHAIN_ID = 137  # Polygon mainnet


class PolymarketClient:
    """Wrapper around py-clob-client for order signing and placement."""

    def __init__(self) -> None:
        self._client = None
        self._initialized = False

    def initialize(self) -> bool:
        """Initialize the CLOB client. Returns True if successful.

        Fails gracefully if private key is not set or py-clob-client
        is not available.
        """
        private_key = os.getenv("POLYMARKET_WALLET_PRIVATE_KEY")
        if not private_key:
            logger.warning("POLYMARKET_WALLET_PRIVATE_KEY not set — client not initialized")
            return False

        try:
            from py_clob_client.client import ClobClient

            self._client = ClobClient(
                CLOB_HOST,
                key=private_key,
                chain_id=CHAIN_ID,
                signature_type=0,  # EOA wallet
            )
            # Derive API credentials
            self._client.set_api_creds(self._client.create_or_derive_api_creds())
            self._initialized = True
            logger.info("Polymarket CLOB client initialized successfully")
            return True
        except ImportError:
            logger.warning("py-clob-client not installed — client not initialized")
            return False
        except Exception as e:
            logger.error("Failed to initialize CLOB client: %s", e)
            return False

    @property
    def is_initialized(self) -> bool:
        return self._initialized

    @property
    def client(self) -> Optional[object]:
        """Access the underlying ClobClient. None if not initialized."""
        return self._client

    def place_order(
        self, token_id: str, price: float, size: float, side: str
    ) -> dict[str, Any]:
        """Place a limit order on the CLOB.

        Args:
            token_id: The token to trade.
            price: Limit price (0-1).
            size: Number of shares.
            side: "BUY" or "SELL".

        Returns:
            Dict with order_id and status.

        Raises:
            RuntimeError: If client is not initialized.
        """
        if not self._initialized or self._client is None:
            raise RuntimeError("Polymarket client not initialized")

        from py_clob_client.order import OrderArgs

        order_args = OrderArgs(
            price=price,
            size=size,
            side=side,
            token_id=token_id,
        )

        signed_order = self._client.create_order(order_args)
        response = self._client.post_order(signed_order, order_type="GTC")

        order_id = response.get("orderID", response.get("order_id", "unknown"))
        status = response.get("status", "submitted")

        logger.info(
            "Order placed: %s %s %.4f @ %.4f → %s (%s)",
            side, token_id[:12], size, price, order_id, status,
        )
        return {"order_id": order_id, "status": status}
