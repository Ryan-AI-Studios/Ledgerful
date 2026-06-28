import { describe, it, expect, vi } from 'vitest'
import { findExistingComment, upsertComment } from '../src/github.js'
import { COMMENT_MARKER } from '../src/comment.js'

function makeOctokit(
  comments: Array<{ id: number; body: string }>,
  createUrl = 'https://github.com/test/pr/1#comment-1',
  updateUrl = 'https://github.com/test/pr/1#comment-99',
) {
  const paginateIterator = vi.fn(async function* () {
    yield { data: comments.map(c => ({ id: c.id, body: c.body })) }
  })
  return {
    paginate: { iterator: paginateIterator },
    rest: {
      issues: {
        listComments: {},
        createComment: vi.fn().mockResolvedValue({ data: { html_url: createUrl, id: 1 } }),
        updateComment: vi.fn().mockResolvedValue({ data: { html_url: updateUrl, id: 99 } }),
      },
    },
  } as any
}

describe('findExistingComment', () => {
  it('returns null when no marker found', async () => {
    const octokit = makeOctokit([
      { id: 1, body: 'Some other comment' },
      { id: 2, body: 'Another comment' },
    ])
    const result = await findExistingComment(octokit, 'owner', 'repo', 42)
    expect(result).toBeNull()
  })

  it('returns ID when marker found', async () => {
    const octokit = makeOctokit([
      { id: 1, body: 'Some other comment' },
      { id: 99, body: `${COMMENT_MARKER}\n## Ledgerful Risk Analysis` },
    ])
    const result = await findExistingComment(octokit, 'owner', 'repo', 42)
    expect(result).toBe(99)
  })
})

describe('upsertComment', () => {
  it('calls createComment when no existing comment', async () => {
    const octokit = makeOctokit([])
    await upsertComment(octokit, 'owner', 'repo', 42, 'body text')
    expect(octokit.rest.issues.createComment).toHaveBeenCalledOnce()
    expect(octokit.rest.issues.updateComment).not.toHaveBeenCalled()
  })

  it('calls updateComment when existing comment found', async () => {
    const octokit = makeOctokit([
      { id: 99, body: `${COMMENT_MARKER}\nold content` },
    ])
    await upsertComment(octokit, 'owner', 'repo', 42, 'new body')
    expect(octokit.rest.issues.updateComment).toHaveBeenCalledOnce()
    expect(octokit.rest.issues.createComment).not.toHaveBeenCalled()
  })

  it('returns html_url from createComment', async () => {
    const octokit = makeOctokit([], 'https://github.com/test/pr/1#comment-1')
    const url = await upsertComment(octokit, 'owner', 'repo', 42, 'body')
    expect(url).toBe('https://github.com/test/pr/1#comment-1')
  })

  it('returns html_url from updateComment', async () => {
    const octokit = makeOctokit(
      [{ id: 99, body: `${COMMENT_MARKER}\nold content` }],
      'https://github.com/test/pr/1#comment-1',
      'https://github.com/test/pr/1#comment-99',
    )
    const url = await upsertComment(octokit, 'owner', 'repo', 42, 'new body')
    expect(url).toBe('https://github.com/test/pr/1#comment-99')
  })
})
