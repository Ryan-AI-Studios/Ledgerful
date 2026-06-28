import type { getOctokit } from '@actions/github'

type Octokit = ReturnType<typeof getOctokit>

import { COMMENT_MARKER } from './comment.js'

export async function findExistingComment(
  octokit: Octokit,
  owner: string,
  repo: string,
  issueNumber: number,
): Promise<number | null> {
  for await (const response of octokit.paginate.iterator(
    octokit.rest.issues.listComments,
    { owner, repo, issue_number: issueNumber, per_page: 100 },
  )) {
    for (const comment of response.data) {
      if (comment.body?.includes(COMMENT_MARKER)) {
        return comment.id
      }
    }
  }
  return null
}

export async function upsertComment(
  octokit: Octokit,
  owner: string,
  repo: string,
  issueNumber: number,
  body: string,
): Promise<string> {
  const existingId = await findExistingComment(octokit, owner, repo, issueNumber)
  if (existingId !== null) {
    const { data } = await octokit.rest.issues.updateComment({
      owner,
      repo,
      comment_id: existingId,
      body,
    })
    return data.html_url
  }
  const { data } = await octokit.rest.issues.createComment({
    owner,
    repo,
    issue_number: issueNumber,
    body,
  })
  return data.html_url
}
