TURNS = [
    "What does pVisor own?",
    "Where is the captured trajectory?",
]

REPLIES = [
    "pVisor owns one Run and its Attempt lifecycle.",
    "Gateway writes the visible dialogue into the Run workspace.",
]

REPLY_BY_USER = dict(zip(TURNS, REPLIES, strict=True))
