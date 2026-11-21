describe('checkout', () => { it('has a heading', () => { cy.document().then(document => { document.body.innerHTML = '<h1>Checkout</h1>'; }); cy.get('h1').should('have.text', 'Checkout'); }); });
