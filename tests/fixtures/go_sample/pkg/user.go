package pkg

// User is a JSON-tagged domain model for fixture coverage.
type User struct {
	ID    int    `json:"id"`
	Name  string `json:"name"`
	Email string `json:"email,omitempty"`
}

// DisplayName returns a friendly name for the user.
func (u *User) DisplayName() string {
	if u.Name == "" {
		return "anonymous"
	}
	return u.Name
}

// IsValid reports whether the user has a positive id and non-empty name.
func (u User) IsValid() bool {
	return u.ID > 0 && u.Name != ""
}

const MaxUsers = 1000

var DefaultUser = User{ID: 0, Name: "guest"}
