.PHONY: help dev backend frontend test lint e2e docker up down
help:        ## list commands
	@grep -E '^[a-z-]+:.*##' $(MAKEFILE_LIST) | awk -F':.*## ' '{printf "  make %-10s %s\n", $$1, $$2}'
backend:     ## run the Rust API on :8080
	cd backend && cargo run
frontend:    ## run the web app on :3000
	cd frontend && npm run dev
test:        ## backend unit tests + frontend type check
	cd backend && cargo test
	cd frontend && npx tsc --noEmit
lint:        ## frontend lint
	cd frontend && npm run lint
e2e:         ## API end-to-end suite against fake Clerk/OpenAI (needs: pip install pyjwt cryptography)
	cd backend && cargo build && cd .. && bash dev/run_e2e.sh
docker:      ## build both images
	docker compose build
up:          ## start the whole project in Docker
	docker compose up --build -d
down:        ## stop it
	docker compose down
