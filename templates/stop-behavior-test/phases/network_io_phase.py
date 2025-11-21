def network_io_phase(test_api, ui):
    """Blocking network request - will block during HTTP request"""
    print("[Network I/O] Starting network request with 30 second delay")
    print("[Network I/O] Try stopping this - urlopen() blocks SIGTERM")
    try:
        import urllib.request

        response = urllib.request.urlopen("https://httpbin.org/delay/30", timeout=60)
        data = response.read()
        print(f"[Network I/O] Received {len(data)} bytes")
    except Exception as e:
        print(f"[Network I/O] Error: {e}")
