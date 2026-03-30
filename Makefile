.PHONY: release-skills

release-skills:
	@test -n "$(RELEASE_VERSION)" || (echo "usage: make release-skills RELEASE_VERSION=0.2.38" && exit 1)
	./scripts/release-skills.sh "$(RELEASE_VERSION)"
