.PHONY: release-skills

release-skills:
	@test -n "$(RELEASE_VERSION)" || (echo "usage: make release-skills RELEASE_VERSION=X.Y.Z" && exit 1)
	./scripts/release-skills.sh "$(RELEASE_VERSION)"
