def plan():
    for value in range(6):
        yield {"id": f"square-{value}", "value": value}


def execute(item):
    value = item["value"]
    return {"square": value * value}
