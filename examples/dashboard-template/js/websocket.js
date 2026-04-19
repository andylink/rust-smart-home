/**
 * websocket.js — WebSocket connection manager with exponential-backoff reconnect
 *
 * Exposes: createWebSocketManager({ url, onMessage, onStatusChange }) → { connect, disconnect }
 *
 * Usage:
 *   var ws = createWebSocketManager({
 *     url:            'ws://127.0.0.1:3001/events?token=...',
 *     onMessage:      function(payload) { ... },
 *     onStatusChange: function(status) { ... },
 *   })
 *   ws.connect()
 *   ws.disconnect()
 *
 * Status values: 'connecting' | 'live' | 'reconnecting' | 'offline' | 'error'
 *
 * Reconnect schedule (capped at 16 s):
 *   attempt 1 →  2 s
 *   attempt 2 →  4 s
 *   attempt 3 →  8 s
 *   attempt 4+ → 16 s
 */

function createWebSocketManager(opts) {
  var url            = opts.url
  var onMessage      = opts.onMessage      || function() {}
  var onStatusChange = opts.onStatusChange || function() {}

  var socket          = null
  var reconnectTimer  = null
  var attempt         = 0
  var intentionalClose = false

  function setStatus(status) {
    onStatusChange(status)
  }

  function connect() {
    intentionalClose = false
    clearTimeout(reconnectTimer)

    setStatus(attempt === 0 ? 'connecting' : 'reconnecting')

    socket = new WebSocket(url)

    socket.onopen = function() {
      attempt = 0
      setStatus('live')
    }

    socket.onmessage = function(event) {
      try {
        var payload = JSON.parse(event.data)
        onMessage(payload)
      } catch (_) {
        // Malformed frame — ignore silently
      }
    }

    socket.onerror = function() {
      setStatus('error')
    }

    socket.onclose = function() {
      socket = null
      if (intentionalClose) return
      setStatus('offline')
      scheduleReconnect()
    }
  }

  function disconnect() {
    intentionalClose = true
    clearTimeout(reconnectTimer)
    if (socket) {
      socket.onclose = null
      socket.close()
      socket = null
    }
    attempt = 0
    setStatus('offline')
  }

  function scheduleReconnect() {
    attempt += 1
    var delayMs = Math.min(1000 * Math.pow(2, Math.min(attempt, 4)), 16000)
    reconnectTimer = setTimeout(connect, delayMs)
  }

  return { connect: connect, disconnect: disconnect }
}
