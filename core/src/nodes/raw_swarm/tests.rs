// Copyright 2019 Parity Technologies (UK) Ltd.
//
// Permission is hereby granted, free of charge, to any person obtaining a
// copy of this software and associated documentation files (the "Software"),
// to deal in the Software without restriction, including without limitation
// the rights to use, copy, modify, merge, publish, distribute, sublicense,
// and/or sell copies of the Software, and to permit persons to whom the
// Software is furnished to do so, subject to the following conditions:
//
// The above copyright notice and this permission notice shall be included in
// all copies or substantial portions of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS
// OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
// FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
// AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
// LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING
// FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER
// DEALINGS IN THE SOFTWARE.

#![cfg(test)]

use super::*;
use crate::tests::dummy_transport::DummyTransport;
use crate::tests::dummy_handler::{Handler, HandlerState, InEvent, OutEvent};
use crate::tests::dummy_transport::ListenerState;
use crate::tests::dummy_muxer::{DummyMuxer, DummyConnectionState};
use crate::nodes::NodeHandlerEvent;
use crate::transport::ListenerEvent;
use assert_matches::assert_matches;
use parking_lot::Mutex;
use std::sync::Arc;
use tokio::runtime::{Builder, Runtime};

#[test]
fn query_transport() {
    let transport = DummyTransport::new();
    let transport2 = transport.clone();
    let raw_swarm = RawSwarm::<_, _, _, Handler, _>::new(transport, PeerId::random());
    assert_eq!(raw_swarm.transport(), &transport2);
}

#[test]
fn local_node_peer() {
    let peer_id = PeerId::random();
    let mut raw_swarm = RawSwarm::<_, _, _, Handler, _>::new(DummyTransport::new(), peer_id.clone());
    assert_matches!(raw_swarm.peer(peer_id), Peer::LocalNode);
}

#[test]
fn nat_traversal_transforms_the_observed_address_according_to_the_transport_used() {
    let addr1 = "/ip4/127.0.0.1/tcp/1234".parse::<Multiaddr>().expect("bad multiaddr");
    // An unrelated outside address is returned as-is, no transform
    let outside_addr1 = "/memory/0".parse::<Multiaddr>().expect("bad multiaddr");

    let addr2 = "/ip4/127.0.0.2/tcp/1234".parse::<Multiaddr>().expect("bad multiaddr");
    let outside_addr2 = "/ip4/127.0.0.2/tcp/1234".parse::<Multiaddr>().expect("bad multiaddr");

    // the DummyTransport nat_traversal increments the port number by one for Ip4 addresses
    let mut transport = DummyTransport::new();
    let events = vec![
        ListenerEvent::NewAddress(addr1.clone()),
        ListenerEvent::NewAddress(addr2.clone())
    ];
    transport.set_initial_listener_state(ListenerState::Events(events));

    let mut raw_swarm = RawSwarm::<_, _, _, Handler, _>::new(transport, PeerId::random());

    raw_swarm.listen_on("/ip4/0.0.0.0/tcp/0".parse().unwrap()).unwrap();

    let raw_swarm =
        future::lazy(move || {
            assert_matches!(raw_swarm.poll(), Async::Ready(RawSwarmEvent::NewListenerAddress {..}));
            assert_matches!(raw_swarm.poll(), Async::Ready(RawSwarmEvent::NewListenerAddress {..}));
            Ok::<_, void::Void>(raw_swarm)
        })
        .wait()
        .unwrap();

    let natted = raw_swarm
        .nat_traversal(&outside_addr1)
        .map(|a| a.to_string())
        .collect::<Vec<_>>();

    assert!(natted.is_empty());

    let natted = raw_swarm
        .nat_traversal(&outside_addr2)
        .map(|a| a.to_string())
        .collect::<Vec<_>>();

    assert_eq!(natted, vec!["/ip4/127.0.0.2/tcp/1234"])
}

#[test]
fn successful_dial_reaches_a_node() {
    let mut swarm = RawSwarm::<_, _, _, Handler, _>::new(DummyTransport::new(), PeerId::random());
    let addr = "/ip4/127.0.0.1/tcp/1234".parse::<Multiaddr>().expect("bad multiaddr");
    let dial_res = swarm.dial(addr, Handler::default());
    assert!(dial_res.is_ok());

    // Poll the swarm until we get a `NodeReached` then assert on the peer:
    // it's there and it's connected.
    let swarm = Arc::new(Mutex::new(swarm));

    let mut rt = Runtime::new().unwrap();
    let mut peer_id : Option<PeerId> = None;
    // Drive forward until we're Connected
    while peer_id.is_none() {
        let swarm_fut = swarm.clone();
        peer_id = rt.block_on(future::poll_fn(move || -> Poll<Option<PeerId>, ()> {
            let mut swarm = swarm_fut.lock();
            let poll_res = swarm.poll();
            match poll_res {
                Async::Ready(RawSwarmEvent::Connected { conn_info, .. }) => Ok(Async::Ready(Some(conn_info))),
                _ => Ok(Async::Ready(None))
            }
        })).expect("tokio works");
    }

    let mut swarm = swarm.lock();
    let peer = swarm.peer(peer_id.unwrap());
    assert_matches!(peer, Peer::Connected(PeerConnected{..}));
}

#[test]
fn num_incoming_negotiated() {
    let mut transport = DummyTransport::new();
    let peer_id = PeerId::random();
    let muxer = DummyMuxer::new();

    let events = vec![
        ListenerEvent::NewAddress("/ip4/127.0.0.1/tcp/1234".parse().unwrap()),
        ListenerEvent::Upgrade {
            upgrade: (peer_id.clone(), muxer.clone()),
            listen_addr: "/ip4/127.0.0.1/tcp/1234".parse().unwrap(),
            remote_addr: "/ip4/127.0.0.1/tcp/32111".parse().unwrap()
        }
    ];
    transport.set_initial_listener_state(ListenerState::Events(events));

    let mut swarm = RawSwarm::<_, _, _, Handler, _>::new(transport, PeerId::random());
    swarm.listen_on("/memory/0".parse().unwrap()).unwrap();

    // no incoming yet
    assert_eq!(swarm.incoming_negotiated().count(), 0);

    let mut rt = Runtime::new().unwrap();
    let swarm = Arc::new(Mutex::new(swarm));
    let swarm_fut = swarm.clone();
    let fut = future::poll_fn(move || -> Poll<_, ()> {
        let mut swarm_fut = swarm_fut.lock();
        assert_matches!(swarm_fut.poll(), Async::Ready(RawSwarmEvent::NewListenerAddress {..}));
        assert_matches!(swarm_fut.poll(), Async::Ready(RawSwarmEvent::IncomingConnection(incoming)) => {
            incoming.accept(Handler::default());
        });
        Ok(Async::Ready(()))
    });
    rt.block_on(fut).expect("tokio works");
    let swarm = swarm.lock();
    // Now there's an incoming connection
    assert_eq!(swarm.incoming_negotiated().count(), 1);
}

#[test]
fn broadcasted_events_reach_active_nodes() {
    let mut swarm = RawSwarm::<_, _, _, Handler, _>::new(DummyTransport::new(), PeerId::random());
    let mut muxer = DummyMuxer::new();
    muxer.set_inbound_connection_state(DummyConnectionState::Pending);
    muxer.set_outbound_connection_state(DummyConnectionState::Opened);
    let addr = "/ip4/127.0.0.1/tcp/1234".parse::<Multiaddr>().expect("bad multiaddr");
    let mut handler = Handler::default();
    handler.next_states = vec![HandlerState::Ready(NodeHandlerEvent::Custom(OutEvent::Custom("from handler 1") )),];
    let dial_result = swarm.dial(addr, handler);
    assert!(dial_result.is_ok());

    swarm.broadcast_event(&InEvent::NextState);
    let swarm = Arc::new(Mutex::new(swarm));
    let mut rt = Runtime::new().unwrap();
    let mut peer_id : Option<PeerId> = None;
    while peer_id.is_none() {
        let swarm_fut = swarm.clone();
        peer_id = rt.block_on(future::poll_fn(move || -> Poll<Option<PeerId>, ()> {
            let mut swarm = swarm_fut.lock();
            let poll_res = swarm.poll();
            match poll_res {
                Async::Ready(RawSwarmEvent::Connected { conn_info, .. }) => Ok(Async::Ready(Some(conn_info))),
                _ => Ok(Async::Ready(None))
            }
        })).expect("tokio works");
    }

    let mut keep_polling = true;
    while keep_polling {
        let swarm_fut = swarm.clone();
        keep_polling = rt.block_on(future::poll_fn(move || -> Poll<_, ()> {
            let mut swarm = swarm_fut.lock();
            match swarm.poll() {
                Async::Ready(event) => {
                    assert_matches!(event, RawSwarmEvent::NodeEvent { conn_info: _, event: inner_event } => {
                        // The event we sent reached the node and triggered sending the out event we told it to return
                        assert_matches!(inner_event, OutEvent::Custom("from handler 1"));
                    });
                    Ok(Async::Ready(false))
                },
                _ => Ok(Async::Ready(true))
            }
        })).expect("tokio works");
    }
}

#[test]
fn querying_for_pending_peer() {
    let mut swarm = RawSwarm::<_, _, _, Handler, _>::new(DummyTransport::new(), PeerId::random());
    let peer_id = PeerId::random();
    let peer = swarm.peer(peer_id.clone());
    assert_matches!(peer, Peer::NotConnected(PeerNotConnected{ .. }));
    let addr = "/memory/0".parse().expect("bad multiaddr");
    let pending_peer = peer.into_not_connected().unwrap().connect(addr, Handler::default());
    assert_matches!(pending_peer, PeerPendingConnect { .. });
}

#[test]
fn querying_for_unknown_peer() {
    let mut swarm = RawSwarm::<_, _, _, Handler, _>::new(DummyTransport::new(), PeerId::random());
    let peer_id = PeerId::random();
    let peer = swarm.peer(peer_id.clone());
    assert_matches!(peer, Peer::NotConnected( PeerNotConnected { nodes: _, peer_id: node_peer_id }) => {
        assert_eq!(node_peer_id, peer_id);
    });
}

#[test]
fn querying_for_connected_peer() {
    let mut swarm = RawSwarm::<_, _, _, Handler, _>::new(DummyTransport::new(), PeerId::random());

    // Dial a node
    let addr = "/ip4/127.0.0.1/tcp/1234".parse().expect("bad multiaddr");
    swarm.dial(addr, Handler::default()).expect("dialing works");

    let swarm = Arc::new(Mutex::new(swarm));
    let mut rt = Runtime::new().unwrap();
    // Drive it forward until we connect; extract the new PeerId.
    let mut peer_id : Option<PeerId> = None;
    while peer_id.is_none() {
        let swarm_fut = swarm.clone();
        peer_id = rt.block_on(future::poll_fn(move || -> Poll<Option<PeerId>, ()> {
            let mut swarm = swarm_fut.lock();
            let poll_res = swarm.poll();
            match poll_res {
                Async::Ready(RawSwarmEvent::Connected { conn_info, .. }) => Ok(Async::Ready(Some(conn_info))),
                _ => Ok(Async::Ready(None))
            }
        })).expect("tokio works");
    }

    // We're connected.
    let mut swarm = swarm.lock();
    let peer = swarm.peer(peer_id.unwrap());
    assert_matches!(peer, Peer::Connected( PeerConnected { .. } ));
}

#[test]
fn poll_with_closed_listener() {
    let mut transport = DummyTransport::new();
    // Set up listener to be closed
    transport.set_initial_listener_state(ListenerState::Ok(Async::Ready(None)));

    let mut swarm = RawSwarm::<_, _, _, Handler, _>::new(transport, PeerId::random());
    swarm.listen_on("/memory/0".parse().unwrap()).unwrap();

    let mut rt = Runtime::new().unwrap();
    let swarm = Arc::new(Mutex::new(swarm));

    let swarm_fut = swarm.clone();
    let fut = future::poll_fn(move || -> Poll<_, ()> {
        let mut swarm = swarm_fut.lock();
        assert_matches!(swarm.poll(), Async::Ready(RawSwarmEvent::ListenerClosed { .. } ));
        Ok(Async::Ready(()))
    });
    rt.block_on(fut).expect("tokio works");
}

#[test]
fn unknown_peer_that_is_unreachable_yields_unknown_peer_dial_error() {
    let mut transport = DummyTransport::new();
    transport.make_dial_fail();
    let mut swarm = RawSwarm::<_, _, _, Handler, _>::new(transport, PeerId::random());
    let addr = "/memory/0".parse::<Multiaddr>().expect("bad multiaddr");
    let handler = Handler::default();
    let dial_result = swarm.dial(addr, handler);
    assert!(dial_result.is_ok());

    let swarm = Arc::new(Mutex::new(swarm));
    let mut rt = Runtime::new().unwrap();
    // Drive it forward until we hear back from the node.
    let mut keep_polling = true;
    while keep_polling {
        let swarm_fut = swarm.clone();
        keep_polling = rt.block_on(future::poll_fn(move || -> Poll<_, ()> {
            let mut swarm = swarm_fut.lock();
            match swarm.poll() {
                Async::NotReady => Ok(Async::Ready(true)),
                Async::Ready(event) => {
                    assert_matches!(event, RawSwarmEvent::UnknownPeerDialError { .. } );
                    Ok(Async::Ready(false))
                },
            }
        })).expect("tokio works");
    }
}

#[test]
fn known_peer_that_is_unreachable_yields_dial_error() {
    let mut transport = DummyTransport::new();
    let peer_id = PeerId::random();
    transport.set_next_peer_id(&peer_id);
    transport.make_dial_fail();
    let swarm = Arc::new(Mutex::new(RawSwarm::<_, _, _, Handler, _>::new(transport, PeerId::random())));

    {
        let swarm1 = swarm.clone();
        let mut swarm1 = swarm1.lock();
        let peer = swarm1.peer(peer_id.clone());
        assert_matches!(peer, Peer::NotConnected(PeerNotConnected{ .. }));
        let addr = "/memory/0".parse::<Multiaddr>().expect("bad multiaddr");
        let pending_peer = peer.into_not_connected().unwrap().connect(addr, Handler::default());
        assert_matches!(pending_peer, PeerPendingConnect { .. });
    }
    let mut rt = Runtime::new().unwrap();
    // Drive it forward until we hear back from the node.
    let mut keep_polling = true;
    while keep_polling {
        let swarm_fut = swarm.clone();
        let peer_id = peer_id.clone();
        keep_polling = rt.block_on(future::poll_fn(move || -> Poll<_, ()> {
            let mut swarm = swarm_fut.lock();
            match swarm.poll() {
                Async::NotReady => Ok(Async::Ready(true)),
                Async::Ready(event) => {
                    let failed_peer_id = assert_matches!(
                        event,
                        RawSwarmEvent::DialError { new_state: _, peer_id: failed_peer_id, .. } => failed_peer_id
                    );
                    assert_eq!(peer_id, failed_peer_id);
                    Ok(Async::Ready(false))
                },
            }
        })).expect("tokio works");
    }
}

#[test]
fn yields_node_error_when_there_is_an_error_after_successful_connect() {
    let mut transport = DummyTransport::new();
    let peer_id = PeerId::random();
    transport.set_next_peer_id(&peer_id);
    let swarm = Arc::new(Mutex::new(RawSwarm::<_, _, _, Handler, _>::new(transport, PeerId::random())));

    {
        // Set up an outgoing connection with a PeerId we know
        let swarm1 = swarm.clone();
        let mut swarm1 = swarm1.lock();
        let peer = swarm1.peer(peer_id.clone());
        let addr = "/unix/reachable".parse().expect("bad multiaddr");
        let mut handler = Handler::default();
        // Force an error
        handler.next_states = vec![ HandlerState::Err ];
        peer.into_not_connected().unwrap().connect(addr, handler);
    }

    // Ensure we run on a single thread
    let mut rt = Builder::new().core_threads(1).build().unwrap();

    // Drive it forward until we connect to the node.
    let mut keep_polling = true;
    while keep_polling {
        let swarm_fut = swarm.clone();
        keep_polling = rt.block_on(future::poll_fn(move || -> Poll<_, ()> {
            let mut swarm = swarm_fut.lock();
            // Push the Handler into an error state on the next poll
            swarm.broadcast_event(&InEvent::NextState);
            match swarm.poll() {
                Async::NotReady => Ok(Async::Ready(true)),
                Async::Ready(event) => {
                    assert_matches!(event, RawSwarmEvent::Connected { .. });
                    // We're connected, we can move on
                    Ok(Async::Ready(false))
                },
            }
        })).expect("tokio works");
    }

    // Poll again. It is going to be a NodeClosed because of how the
    // handler's next state was set up.
    let swarm_fut = swarm.clone();
    let expected_peer_id = peer_id.clone();
    rt.block_on(future::poll_fn(move || -> Poll<_, ()> {
        let mut swarm = swarm_fut.lock();
        assert_matches!(swarm.poll(), Async::Ready(RawSwarmEvent::NodeClosed { conn_info, .. }) => {
            assert_eq!(conn_info, expected_peer_id);
        });
        Ok(Async::Ready(()))
    })).expect("tokio works");
}

#[test]
fn local_prio_equivalence_relation() {
    for _ in 0..1000 {
        let a = PeerId::random();
        let b = PeerId::random();
        assert_ne!(has_dial_prio(&a, &b), has_dial_prio(&b, &a));
    }
}

#[test]
fn limit_incoming_connections() {
    let mut transport = DummyTransport::new();
    let peer_id = PeerId::random();
    let muxer = DummyMuxer::new();
    let limit = 1;

    let mut events = vec![ListenerEvent::NewAddress("/ip4/127.0.0.1/tcp/1234".parse().unwrap())];
    events.extend(std::iter::repeat(
        ListenerEvent::Upgrade {
            upgrade: (peer_id.clone(), muxer.clone()),
            listen_addr: "/ip4/127.0.0.1/tcp/1234".parse().unwrap(),
            remote_addr: "/ip4/127.0.0.1/tcp/32111".parse().unwrap()
        }
    ).take(10));
    transport.set_initial_listener_state(ListenerState::Events(events));

    let mut swarm = RawSwarm::<_, _, _, Handler, _>::new_with_incoming_limit(transport, PeerId::random(), Some(limit));
    assert_eq!(swarm.incoming_limit(), Some(limit));
    swarm.listen_on("/memory/0".parse().unwrap()).unwrap();
    assert_eq!(swarm.incoming_negotiated().count(), 0);

    let swarm = Arc::new(Mutex::new(swarm));
    let mut rt = Runtime::new().unwrap();
    for i in 1..10 {
        let swarm_fut = swarm.clone();
        let fut = future::poll_fn(move || -> Poll<_, ()> {
            let mut swarm_fut = swarm_fut.lock();
            if i <= limit {
                assert_matches!(swarm_fut.poll(), Async::Ready(RawSwarmEvent::NewListenerAddress {..}));
                assert_matches!(swarm_fut.poll(),
                    Async::Ready(RawSwarmEvent::IncomingConnection(incoming)) => {
                        incoming.accept(Handler::default());
                });
            } else {
                match swarm_fut.poll() {
                    Async::NotReady => (),
                    Async::Ready(x) => {
                        match x {
                            RawSwarmEvent::NewListenerAddress {..} => {}
                            RawSwarmEvent::ExpiredListenerAddress {..} => {}
                            RawSwarmEvent::IncomingConnection(_) => {}
                            RawSwarmEvent::Connected {..} => {}
                            e => panic!("Not expected event: {:?}", e)
                        }
                    },
                }
             }
            Ok(Async::Ready(()))
        });
        rt.block_on(fut).expect("tokio works");
        let swarm = swarm.lock();
        assert!(swarm.incoming_negotiated().count() <= (limit as usize));
    }
}
