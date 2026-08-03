def plan():
    for index in range(3):
        yield {
            "id": f"trajectory-{index}",
            "agent": "example-agent",
            "command": ["/bin/sh", "-c", f"printf trajectory-{index}"],
        }
