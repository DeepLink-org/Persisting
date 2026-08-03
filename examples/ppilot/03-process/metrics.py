def map(records, context):
    return {
        "trajectories": len(records),
        "steps": sum(len(record["steps"]) for record in records),
    }


def reduce(partials, context):
    return {
        "trajectories": sum(partial["trajectories"] for partial in partials),
        "steps": sum(partial["steps"] for partial in partials),
        "mappers": context["mapper_count"],
    }
