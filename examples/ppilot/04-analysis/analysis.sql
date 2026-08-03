SELECT session_id, COUNT(*) AS steps
FROM steps
GROUP BY session_id
ORDER BY session_id
