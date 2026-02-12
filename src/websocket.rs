use axum::{
    extract::{
        ws::{Message, WebSocket},
        State, WebSocketUpgrade,
    },
    response::IntoResponse,
};
use serde::Serialize;
use tokio::sync::broadcast;
use tracing::{debug, warn};

/// Events pushed to connected dashboard clients via WebSocket.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DashboardEvent {
    CycleComplete {
        cycle_number: i64,
        bankroll: f64,
        exposure: f64,
        trades_placed: u32,
        api_cost: f64,
        positions_checked: u32,
    },
    TradeExecuted {
        trade_id: String,
        market_id: String,
        side: String,
        price: f64,
        size: f64,
        paper: bool,
    },
    PositionExit {
        market_id: String,
        side: String,
        exit_price: f64,
        pnl: f64,
        reason: String,
    },
    PositionAlert {
        market_id: String,
        alert_type: String,
        details: String,
    },
}

pub type EventSender = broadcast::Sender<DashboardEvent>;

/// Create a new broadcast channel for dashboard events.
pub fn new_event_channel() -> EventSender {
    let (tx, _) = broadcast::channel(64);
    tx
}

/// Axum handler: upgrade HTTP to WebSocket, then forward events.
pub async fn ws_handler(ws: WebSocketUpgrade, State(tx): State<EventSender>) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_ws(socket, tx))
}

async fn handle_ws(mut socket: WebSocket, tx: EventSender) {
    let mut rx = tx.subscribe();
    debug!("Dashboard WebSocket client connected");

    loop {
        tokio::select! {
            // Forward broadcast events to client
            event = rx.recv() => {
                match event {
                    Ok(ev) => {
                        let json = match serde_json::to_string(&ev) {
                            Ok(j) => j,
                            Err(e) => {
                                warn!("Failed to serialize dashboard event: {}", e);
                                continue;
                            }
                        };
                        if socket.send(Message::Text(json)).await.is_err() {
                            break; // Client disconnected
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        warn!("Dashboard WS client lagged, skipped {} events", n);
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        break; // Channel closed
                    }
                }
            }
            // Handle incoming messages (read-only dashboard, just consume/ignore)
            msg = socket.recv() => {
                match msg {
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Ok(Message::Ping(data))) => {
                        if socket.send(Message::Pong(data)).await.is_err() {
                            break;
                        }
                    }
                    Some(Ok(_)) => {} // Ignore text/binary from client
                    Some(Err(_)) => break,
                }
            }
        }
    }
    debug!("Dashboard WebSocket client disconnected");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_event_serialization_cycle_complete() {
        let event = DashboardEvent::CycleComplete {
            cycle_number: 42,
            bankroll: 47.50,
            exposure: 12.30,
            trades_placed: 2,
            api_cost: 0.15,
            positions_checked: 5,
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "cycle_complete");
        assert_eq!(json["cycle_number"], 42);
        assert_eq!(json["bankroll"], 47.5);
    }

    #[test]
    fn test_event_serialization_trade_executed() {
        let event = DashboardEvent::TradeExecuted {
            trade_id: "t1".to_string(),
            market_id: "0xabc".to_string(),
            side: "YES".to_string(),
            price: 0.65,
            size: 3.0,
            paper: true,
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "trade_executed");
        assert_eq!(json["paper"], true);
    }

    #[test]
    fn test_event_serialization_position_exit() {
        let event = DashboardEvent::PositionExit {
            market_id: "0xdef".to_string(),
            side: "NO".to_string(),
            exit_price: 0.80,
            pnl: 1.50,
            reason: "take_profit".to_string(),
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "position_exit");
        assert_eq!(json["reason"], "take_profit");
    }

    #[test]
    fn test_event_serialization_position_alert() {
        let event = DashboardEvent::PositionAlert {
            market_id: "0xghi".to_string(),
            alert_type: "whale_move".to_string(),
            details: "Large opposing trade detected".to_string(),
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "position_alert");
        assert_eq!(json["alert_type"], "whale_move");
    }

    #[test]
    fn test_broadcast_channel_send_receive() {
        let tx = new_event_channel();
        let mut rx = tx.subscribe();

        let event = DashboardEvent::CycleComplete {
            cycle_number: 1,
            bankroll: 50.0,
            exposure: 0.0,
            trades_placed: 0,
            api_cost: 0.01,
            positions_checked: 0,
        };

        tx.send(event.clone()).unwrap();
        let received = rx.try_recv().unwrap();
        assert!(matches!(
            received,
            DashboardEvent::CycleComplete {
                cycle_number: 1,
                ..
            }
        ));
    }

    #[test]
    fn test_broadcast_no_receivers_ok() {
        let tx = new_event_channel();
        // No subscribers — send should return Err but not panic
        let result = tx.send(DashboardEvent::CycleComplete {
            cycle_number: 1,
            bankroll: 50.0,
            exposure: 0.0,
            trades_placed: 0,
            api_cost: 0.0,
            positions_checked: 0,
        });
        assert!(result.is_err()); // No receivers
    }
}
