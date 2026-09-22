## Required model

Use the latest stable Gemini Flash model available from Google's Gemini provider at execution time for every measured agent run. Do not use Flash-Lite, a preview/experimental model, or another model family as a substitute.

Resolve the exact provider/model ID from pi's current model catalog and verify against Google's current model documentation that it is the latest stable Gemini Flash release. Do not assume a hard-coded version or an alias containing `latest` is current. Record the resolved provider/model ID, any alias used, the verification date/source, and the pi version in the campaign report and run metadata. Pin that resolved ID for the entire campaign so repetitions use the same model.

Preflight authentication and model availability before starting the campaign. If the required model is unavailable through the installed pi version or configured credentials, report the blocker rather than silently falling back. Do not switch models to make failing scenarios pass.
